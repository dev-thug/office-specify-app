//! research module repository (spec §6 — owned by the module worker).
//!
//! SQL for `lead_proposals` / `research_evidence` plus the shared-table
//! writes the accept path performs (`companies`, `contacts`,
//! `email_addresses`, `contact_emails`, `communication_bases`).
//!
//! Also hosts `validate_research_url` — the §15.1/§13.5 SSRF input policy
//! for evidence `source_url`s. It lives here (not in `domain`) because a
//! correct check needs a WHATWG URL parser — `reqwest::Url` handles
//! backslash normalization, integer/hex IP literals (`2130706433` →
//! `127.0.0.1`), userinfo and IPv6 bracket syntax; a hand-rolled parser
//! would miss exactly the forms T27 tests. The system never *fetches*
//! these URLs — the check keeps non-public targets out of stored evidence.

use std::net::{Ipv4Addr, Ipv6Addr};

use chrono::{DateTime, Utc};
use contracts::AppError;
use contracts::research::ResearchTargetKind;
use uuid::Uuid;

use crate::db::Tx;
use crate::error::map_sqlx;

// ---------------------------------------------------------------- rows

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct LeadProposalRow {
    pub id: Uuid,
    pub agent_run_id: Option<Uuid>,
    pub proposed_by: Uuid,
    pub external_key: Option<String>,
    pub company_name: String,
    pub domain: Option<String>,
    pub proposed_data: serde_json::Value,
    pub status: String,
    pub accepted_company_id: Option<Uuid>,
    pub reviewed_by: Option<Uuid>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

const PROPOSAL_COLS: &str = "id, agent_run_id, proposed_by, external_key, company_name,
         domain, proposed_data, status, accepted_company_id, reviewed_by,
         reviewed_at, version, created_at";

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct EvidenceRow {
    pub id: Uuid,
    pub company_id: Option<Uuid>,
    pub contact_id: Option<Uuid>,
    pub lead_proposal_id: Option<Uuid>,
    pub field_path: String,
    pub source_url: String,
    pub observed_at: DateTime<Utc>,
    pub excerpt: Option<String>,
    pub value: serde_json::Value,
    pub evidence_type: String,
    /// NUMERIC(4,3) read as float8 — the workspace has no decimal feature.
    pub confidence: Option<f64>,
    pub review_status: String,
    pub created_by: Uuid,
    pub reviewed_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

const EVIDENCE_COLS: &str = "id, company_id, contact_id, lead_proposal_id, field_path,
         source_url, observed_at, excerpt, value, evidence_type,
         confidence::float8 AS confidence,
         review_status, created_by, reviewed_by, created_at";

// -------------------------------------------------------- lead proposals

#[allow(clippy::too_many_arguments)]
pub async fn insert_proposal(
    tx: &mut Tx<'_>,
    agent_run_id: Option<Uuid>,
    proposed_by: Uuid,
    external_key: Option<&str>,
    company_name: &str,
    domain: Option<&str>,
    proposed_data: &serde_json::Value,
) -> Result<Uuid, AppError> {
    sqlx::query_scalar(
        "INSERT INTO lead_proposals
            (workspace_id, agent_run_id, proposed_by, external_key,
             company_name, domain, proposed_data)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(agent_run_id)
    .bind(proposed_by)
    .bind(external_key)
    .bind(company_name)
    .bind(domain)
    .bind(proposed_data)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn get_proposal(tx: &mut Tx<'_>, id: Uuid) -> Result<Option<LeadProposalRow>, AppError> {
    sqlx::query_as::<_, LeadProposalRow>(&format!(
        "SELECT {PROPOSAL_COLS} FROM lead_proposals
          WHERE workspace_id = $1 AND id = $2"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// `FOR UPDATE` variant — the decision path locks before checking version.
pub async fn lock_proposal(tx: &mut Tx<'_>, id: Uuid) -> Result<Option<LeadProposalRow>, AppError> {
    sqlx::query_as::<_, LeadProposalRow>(&format!(
        "SELECT {PROPOSAL_COLS} FROM lead_proposals
          WHERE workspace_id = $1 AND id = $2
          FOR UPDATE"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Keyset page: `(created_at, id) < (after_ts, after_id)`, newest first
/// (B.1.5 default sort).
pub async fn list_proposals(
    tx: &mut Tx<'_>,
    status: Option<&str>,
    after: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
) -> Result<Vec<LeadProposalRow>, AppError> {
    sqlx::query_as::<_, LeadProposalRow>(&format!(
        "SELECT {PROPOSAL_COLS} FROM lead_proposals
          WHERE workspace_id = $1
            AND ($2::text IS NULL OR status = $2)
            AND ($3::timestamptz IS NULL OR (created_at, id) < ($3, $4))
          ORDER BY created_at DESC, id DESC
          LIMIT $5"
    ))
    .bind(tx.workspace_id())
    .bind(status)
    .bind(after.map(|a| a.0))
    .bind(after.map(|a| a.1).unwrap_or(Uuid::nil()))
    .bind(limit)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Terminal decision transition guarded by version + source status.
/// Returns the new version on success.
pub async fn decide_proposal(
    tx: &mut Tx<'_>,
    id: Uuid,
    expected_version: i64,
    new_status: &str,
    accepted_company_id: Option<Uuid>,
    reviewed_by: Uuid,
) -> Result<Option<i64>, AppError> {
    sqlx::query_scalar(
        "UPDATE lead_proposals
            SET status = $4, accepted_company_id = $5,
                reviewed_by = $6, reviewed_at = now(),
                version = version + 1
          WHERE workspace_id = $1 AND id = $2 AND version = $3
            AND status IN ('pending','needs_review')
          RETURNING version",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .bind(expected_version)
    .bind(new_status)
    .bind(accepted_company_id)
    .bind(reviewed_by)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// `agent_runs` principal for a run id in this workspace (proposal
/// `agent_run_id` validation — the run must exist; an agent may only
/// attach runs it owns).
pub async fn agent_run_principal(tx: &mut Tx<'_>, run_id: Uuid) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar(
        "SELECT principal_id FROM agent_runs
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(run_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Duplicate-company candidates (§13.4 — reported, never auto-merged):
/// normalized-domain equality or exact case-insensitive name equality.
/// Non-archived only, capped at 10.
pub async fn find_duplicate_company_ids(
    tx: &mut Tx<'_>,
    normalized_domain: Option<&str>,
    company_name: &str,
) -> Result<Vec<Uuid>, AppError> {
    sqlx::query_scalar(
        "SELECT id FROM companies
          WHERE workspace_id = $1 AND archived_at IS NULL
            AND (($2::text IS NOT NULL AND lower(domain) = $2)
                 OR lower(name) = lower($3))
          ORDER BY created_at
          LIMIT 10",
    )
    .bind(tx.workspace_id())
    .bind(normalized_domain)
    .bind(company_name)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

// -------------------------------------------------------------- evidence

#[allow(clippy::too_many_arguments)]
pub async fn insert_evidence(
    tx: &mut Tx<'_>,
    target: EvidenceTarget,
    field_path: &str,
    source_url: &str,
    observed_at: DateTime<Utc>,
    excerpt: Option<&str>,
    value: &serde_json::Value,
    evidence_type: &str,
    confidence: Option<f64>,
    created_by: Uuid,
) -> Result<Uuid, AppError> {
    let (company_id, contact_id, lead_proposal_id) = match target {
        EvidenceTarget::Company(id) => (Some(id), None, None),
        EvidenceTarget::Contact(id) => (None, Some(id), None),
        EvidenceTarget::LeadProposal(id) => (None, None, Some(id)),
    };
    sqlx::query_scalar(
        "INSERT INTO research_evidence
            (workspace_id, company_id, contact_id, lead_proposal_id,
             field_path, source_url, observed_at, excerpt, value,
             evidence_type, confidence, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(company_id)
    .bind(contact_id)
    .bind(lead_proposal_id)
    .bind(field_path)
    .bind(source_url)
    .bind(observed_at)
    .bind(excerpt)
    .bind(value)
    .bind(evidence_type)
    .bind(confidence)
    .bind(created_by)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

#[derive(Debug, Clone, Copy)]
pub enum EvidenceTarget {
    Company(Uuid),
    Contact(Uuid),
    LeadProposal(Uuid),
}

pub async fn list_evidence_for_proposal(
    tx: &mut Tx<'_>,
    proposal_id: Uuid,
) -> Result<Vec<EvidenceRow>, AppError> {
    sqlx::query_as::<_, EvidenceRow>(&format!(
        "SELECT {EVIDENCE_COLS} FROM research_evidence
          WHERE workspace_id = $1 AND lead_proposal_id = $2
          ORDER BY created_at, id"
    ))
    .bind(tx.workspace_id())
    .bind(proposal_id)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Keyset page over `(created_at, id) < after`, newest first. Target
/// filters are exact ids; `review_status` is a CHECK'd text value.
#[allow(clippy::too_many_arguments)]
pub async fn list_evidence(
    tx: &mut Tx<'_>,
    company_id: Option<Uuid>,
    contact_id: Option<Uuid>,
    lead_proposal_id: Option<Uuid>,
    review_status: Option<&str>,
    after: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
) -> Result<Vec<EvidenceRow>, AppError> {
    sqlx::query_as::<_, EvidenceRow>(&format!(
        "SELECT {EVIDENCE_COLS} FROM research_evidence
          WHERE workspace_id = $1
            AND ($2::uuid IS NULL OR company_id = $2)
            AND ($3::uuid IS NULL OR contact_id = $3)
            AND ($4::uuid IS NULL OR lead_proposal_id = $4)
            AND ($5::text IS NULL OR review_status = $5)
            AND ($6::timestamptz IS NULL OR (created_at, id) < ($6, $7))
          ORDER BY created_at DESC, id DESC
          LIMIT $8"
    ))
    .bind(tx.workspace_id())
    .bind(company_id)
    .bind(contact_id)
    .bind(lead_proposal_id)
    .bind(review_status)
    .bind(after.map(|a| a.0))
    .bind(after.map(|a| a.1).unwrap_or(Uuid::nil()))
    .bind(limit)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn get_evidence(tx: &mut Tx<'_>, id: Uuid) -> Result<Option<EvidenceRow>, AppError> {
    sqlx::query_as::<_, EvidenceRow>(&format!(
        "SELECT {EVIDENCE_COLS} FROM research_evidence
          WHERE workspace_id = $1 AND id = $2"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Review transition — only from `pending` (a terminal row returns 0 rows
/// affected so the caller can 409).
pub async fn review_evidence(
    tx: &mut Tx<'_>,
    id: Uuid,
    new_status: &str,
    reviewed_by: Uuid,
) -> Result<bool, AppError> {
    let n = sqlx::query(
        "UPDATE research_evidence
            SET review_status = $3, reviewed_by = $4
          WHERE workspace_id = $1 AND id = $2 AND review_status = 'pending'",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .bind(new_status)
    .bind(reviewed_by)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(n == 1)
}

// ------------------------------------------------- shared-table writes

/// CRM values captured while a `crm_propose_update` target row is locked.
/// `fields` contains only the MCP proposal allowlist for that target kind.
#[derive(Debug, Clone)]
pub struct UpdateTargetRow {
    pub label: String,
    pub domain: Option<String>,
    pub version: i64,
    pub fields: serde_json::Value,
    /// For a contact target, the associated company if one exists.
    pub company_id: Option<Uuid>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct ContactUpdateTargetDbRow {
    label: String,
    domain: Option<String>,
    version: i64,
    fields: serde_json::Value,
    company_id: Option<Uuid>,
}

/// Lock a company/contact row and snapshot the fields allowed in an update
/// proposal. The caller compares `version` before creating the pending diff.
pub async fn lock_update_target(
    tx: &mut Tx<'_>,
    kind: ResearchTargetKind,
    id: Uuid,
) -> Result<Option<UpdateTargetRow>, AppError> {
    let ws = tx.workspace_id();
    match kind {
        ResearchTargetKind::Company => {
            let row: Option<(String, Option<String>, i64, serde_json::Value)> = sqlx::query_as(
                "SELECT name, domain, version,
                        jsonb_build_object(
                            'name', name, 'domain', domain, 'website_url', website_url,
                            'industry', industry, 'country_code', country_code,
                            'lifecycle_stage', lifecycle_stage, 'description', description
                        )
                   FROM companies
                  WHERE workspace_id = $1 AND id = $2 AND archived_at IS NULL
                  FOR UPDATE",
            )
            .bind(ws)
            .bind(id)
            .fetch_optional(tx.conn())
            .await
            .map_err(map_sqlx)?;
            Ok(row.map(|(label, domain, version, fields)| UpdateTargetRow {
                label,
                domain,
                version,
                fields,
                company_id: Some(id),
            }))
        }
        ResearchTargetKind::Contact => {
            let row = sqlx::query_as::<_, ContactUpdateTargetDbRow>(
                "SELECT c.name AS label, co.domain, c.version,
                        jsonb_build_object(
                            'name', c.name, 'job_title', c.job_title,
                            'department', c.department, 'status', c.status
                        ) AS fields, co.id AS company_id
                   FROM contacts c
                   LEFT JOIN companies co
                     ON co.workspace_id = c.workspace_id AND co.id = c.company_id
                  WHERE c.workspace_id = $1 AND c.id = $2 AND c.archived_at IS NULL
                  FOR UPDATE OF c",
            )
            .bind(ws)
            .bind(id)
            .fetch_optional(tx.conn())
            .await
            .map_err(map_sqlx)?;
            Ok(row.map(|row| UpdateTargetRow {
                label: row.label,
                domain: row.domain,
                version: row.version,
                fields: row.fields,
                company_id: row.company_id,
            }))
        }
    }
}

/// Apply an already revalidated proposal diff. Column names stay fixed in
/// SQL; JSON keys only decide which allowlisted values are assigned. The
/// version predicate is the final stale-write guard at acceptance time.
pub async fn apply_update_target(
    tx: &mut Tx<'_>,
    kind: ResearchTargetKind,
    id: Uuid,
    expected_version: i64,
    changes: &serde_json::Value,
) -> Result<Option<i64>, AppError> {
    let version = match kind {
        ResearchTargetKind::Company => sqlx::query_scalar(
            "UPDATE companies
                SET name = CASE WHEN $4 ? 'name' THEN $4->>'name' ELSE name END,
                    domain = CASE WHEN $4 ? 'domain' THEN $4->>'domain' ELSE domain END,
                    website_url = CASE WHEN $4 ? 'website_url' THEN $4->>'website_url' ELSE website_url END,
                    industry = CASE WHEN $4 ? 'industry' THEN $4->>'industry' ELSE industry END,
                    country_code = CASE WHEN $4 ? 'country_code' THEN $4->>'country_code' ELSE country_code END,
                    lifecycle_stage = CASE WHEN $4 ? 'lifecycle_stage' THEN $4->>'lifecycle_stage' ELSE lifecycle_stage END,
                    description = CASE WHEN $4 ? 'description' THEN $4->>'description' ELSE description END,
                    version = version + 1, updated_at = now()
              WHERE workspace_id = $1 AND id = $2 AND version = $3 AND archived_at IS NULL
              RETURNING version",
        )
        .bind(tx.workspace_id())
        .bind(id)
        .bind(expected_version)
        .bind(changes)
        .fetch_optional(tx.conn())
        .await
        .map_err(map_sqlx)?,
        ResearchTargetKind::Contact => sqlx::query_scalar(
            "UPDATE contacts
                SET name = CASE WHEN $4 ? 'name' THEN $4->>'name' ELSE name END,
                    job_title = CASE WHEN $4 ? 'job_title' THEN $4->>'job_title' ELSE job_title END,
                    department = CASE WHEN $4 ? 'department' THEN $4->>'department' ELSE department END,
                    status = CASE WHEN $4 ? 'status' THEN $4->>'status' ELSE status END,
                    version = version + 1, updated_at = now()
              WHERE workspace_id = $1 AND id = $2 AND version = $3 AND archived_at IS NULL
              RETURNING version",
        )
        .bind(tx.workspace_id())
        .bind(id)
        .bind(expected_version)
        .bind(changes)
        .fetch_optional(tx.conn())
        .await
        .map_err(map_sqlx)?,
    };
    Ok(version)
}

pub async fn company_exists(tx: &mut Tx<'_>, id: Uuid) -> Result<bool, AppError> {
    let found: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM companies WHERE workspace_id = $1 AND id = $2")
            .bind(tx.workspace_id())
            .bind(id)
            .fetch_optional(tx.conn())
            .await
            .map_err(map_sqlx)?;
    Ok(found.is_some())
}

pub async fn contact_exists(tx: &mut Tx<'_>, id: Uuid) -> Result<bool, AppError> {
    let found: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM contacts WHERE workspace_id = $1 AND id = $2")
            .bind(tx.workspace_id())
            .bind(id)
            .fetch_optional(tx.conn())
            .await
            .map_err(map_sqlx)?;
    Ok(found.is_some())
}

#[allow(clippy::too_many_arguments)]
pub async fn insert_company(
    tx: &mut Tx<'_>,
    name: &str,
    domain: Option<&str>,
    industry: Option<&str>,
    country_code: Option<&str>,
    website_url: Option<&str>,
) -> Result<Uuid, AppError> {
    sqlx::query_scalar(
        "INSERT INTO companies
            (workspace_id, name, domain, industry, country_code, website_url)
         VALUES ($1, $2, $3, $4, $5, $6)
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(name)
    .bind(domain)
    .bind(industry)
    .bind(country_code)
    .bind(website_url)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Commit-side company lookup used by the `link` duplicate strategy:
/// exact normalized-domain match, non-archived, oldest first.
pub async fn company_id_by_domain(
    tx: &mut Tx<'_>,
    normalized_domain: &str,
) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar(
        "SELECT id FROM companies
          WHERE workspace_id = $1 AND archived_at IS NULL AND lower(domain) = $2
          ORDER BY created_at LIMIT 1",
    )
    .bind(tx.workspace_id())
    .bind(normalized_domain)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn insert_contact(
    tx: &mut Tx<'_>,
    company_id: Option<Uuid>,
    name: &str,
    job_title: Option<&str>,
    department: Option<&str>,
    owner_principal_id: Option<Uuid>,
) -> Result<Uuid, AppError> {
    sqlx::query_scalar(
        "INSERT INTO contacts
            (workspace_id, company_id, name, job_title, department, owner_principal_id)
         VALUES ($1, $2, $3, $4, $5, $6)
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(company_id)
    .bind(name)
    .bind(job_title)
    .bind(department)
    .bind(owner_principal_id)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Find-or-create on `(workspace_id, normalized_key)` — the dedupe
/// identity. A lost insert race (two imports proposing the same address)
/// re-selects; `address_original` keeps the first-seen form.
pub async fn find_or_create_email_address(
    tx: &mut Tx<'_>,
    original: &str,
    normalized_key: &str,
    normalization_version: i16,
) -> Result<Uuid, AppError> {
    let inserted: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO email_addresses
            (workspace_id, address_original, normalized_key, normalization_version)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (workspace_id, normalized_key) DO NOTHING
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(original)
    .bind(normalized_key)
    .bind(normalization_version)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    if let Some(id) = inserted {
        return Ok(id);
    }
    sqlx::query_scalar(
        "SELECT id FROM email_addresses
          WHERE workspace_id = $1 AND normalized_key = $2",
    )
    .bind(tx.workspace_id())
    .bind(normalized_key)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// `contact_emails` link — `UNIQUE (contact_id, email_address_id)` makes
/// replays idempotent.
pub async fn link_contact_email(
    tx: &mut Tx<'_>,
    contact_id: Uuid,
    email_address_id: Uuid,
    label: Option<&str>,
    is_primary: bool,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO contact_emails
            (workspace_id, contact_id, email_address_id, label, is_primary)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (workspace_id, contact_id, email_address_id) DO NOTHING",
    )
    .bind(tx.workspace_id())
    .bind(contact_id)
    .bind(email_address_id)
    .bind(label)
    .bind(is_primary)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

/// Does a basis row already exist for (address, purpose, jurisdiction)?
/// Any status counts — a second pending basis for the same address is
/// noise.
pub async fn basis_exists(
    tx: &mut Tx<'_>,
    email_address_id: Uuid,
    purpose: &str,
    jurisdiction: &str,
) -> Result<bool, AppError> {
    let found: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM communication_bases
          WHERE workspace_id = $1 AND email_address_id = $2
            AND purpose = $3 AND jurisdiction = $4",
    )
    .bind(tx.workspace_id())
    .bind(email_address_id)
    .bind(purpose)
    .bind(jurisdiction)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(found.is_some())
}

/// A `status='pending'` basis records that the address was *seen* but its
/// send permission is unverified — it never satisfies
/// `valid_communication_basis` (which requires `status='valid'`), so a
/// researched/imported address stays unsendable until human review (§13.4,
/// T35).
pub async fn insert_pending_basis(
    tx: &mut Tx<'_>,
    email_address_id: Uuid,
    purpose: &str,
    jurisdiction: &str,
    basis_type: &str,
    evidence_ref: &str,
) -> Result<Uuid, AppError> {
    sqlx::query_scalar(
        "INSERT INTO communication_bases
            (workspace_id, email_address_id, purpose, jurisdiction,
             basis_type, status, evidence_ref, collected_at)
         VALUES ($1, $2, $3, $4, $5, 'pending', $6, now())
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(email_address_id)
    .bind(purpose)
    .bind(jurisdiction)
    .bind(basis_type)
    .bind(evidence_ref)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// The workspace's system principal — audit `actor_principal_id` for
/// worker-side actions (§5.1 actor vs requested_by).
pub async fn system_principal_id(tx: &mut Tx<'_>) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar(
        "SELECT id FROM principals
          WHERE workspace_id = $1 AND kind = 'system' LIMIT 1",
    )
    .bind(tx.workspace_id())
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

// ------------------------------------------------- research URL policy

/// Why a `source_url` was refused (mapped to field violations upstream).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlRejection {
    /// Unparseable or not absolute.
    Invalid,
    /// Not http/https.
    Scheme,
    /// userinfo present (`user:pass@host`) — leak-prone, never legit.
    Credentials,
    /// Loopback/private/link-local/CGN/reserved IP literal, `localhost`-
    /// family names, single-label intranet hosts (§15.1 SSRF table, T27).
    NotPublicHost,
}

fn v4_is_public(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    // Comprehensive "not globally routable" set — RFC 6890 plus common
    // sense: unspecified, loopback, RFC1918, CGN, link-local, IETF
    // protocol assignments, documentation, benchmarking, multicast,
    // reserved, broadcast.
    !(o[0] == 0
        || o[0] == 10
        || o[0] == 127
        || (o[0] == 100 && (64..=127).contains(&o[1]))
        || (o[0] == 169 && o[1] == 254)
        || (o[0] == 172 && (16..=31).contains(&o[1]))
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)
        || (o[0] == 192 && o[1] == 0 && o[2] == 2)
        || (o[0] == 192 && o[1] == 88 && o[2] == 99)
        || (o[0] == 192 && o[1] == 168)
        || (o[0] == 198 && (o[1] == 18 || o[1] == 19))
        || (o[0] == 198 && o[1] == 51 && o[2] == 100)
        || (o[0] == 203 && o[1] == 0 && o[2] == 113)
        || o[0] >= 224)
}

fn v6_is_public(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return v4_is_public(v4);
    }
    let seg = ip.segments();
    // ::/128 unspecified, ::1 loopback, fc00::/7 ULA, fe80::/10 link-local,
    // ff00::/8 multicast, 2001:db8::/32 documentation, ::ffff:0:0/96 was
    // handled by to_ipv4_mapped above; also reject v4-compatible ::/96.
    !(ip.is_unspecified()
        || ip.is_loopback()
        || (seg[0] & 0xfe00) == 0xfc00
        || (seg[0] & 0xffc0) == 0xfe80
        || (seg[0] & 0xff00) == 0xff00
        || (seg[0] == 0x2001 && seg[1] == 0x0db8)
        || (seg[0] == 0 && seg[1] == 0 && seg[2] == 0 && seg[3] == 0 && seg[4] == 0))
}

fn domain_host_public(host: &str) -> bool {
    let h = host.trim_end_matches('.').to_lowercase();
    if h.is_empty() || h == "localhost" || !h.contains('.') {
        return false;
    }
    // Intranet/reserved suffixes — never a public evidence source.
    ![
        ".localhost",
        ".local",
        ".internal",
        ".intranet",
        ".lan",
        ".home",
        ".corp",
    ]
    .iter()
    .any(|s| h.ends_with(s))
}

/// §15.1/§13.5 input policy: evidence `source_url` must be an absolute
/// public `http(s)` URL — no credentials, no private/loopback/link-local/
/// reserved targets. IP literals are checked after WHATWG normalization
/// (integer/hex/octal forms already resolved by `reqwest::Url`), which is
/// what makes T27's "DNS-rebinding-style" variants fail closed.
pub fn validate_research_url(raw: &str) -> Result<(), UrlRejection> {
    let url = reqwest::Url::parse(raw.trim()).map_err(|_| UrlRejection::Invalid)?;
    match url.scheme() {
        "http" | "https" => {}
        _ => return Err(UrlRejection::Scheme),
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UrlRejection::Credentials);
    }
    let Some(host) = url.host_str() else {
        return Err(UrlRejection::NotPublicHost);
    };
    // WHATWG already normalized exotic IP forms: IPv6 hosts serialize in
    // [brackets], integer/hex/octal IPv4 as dotted decimal.
    if let Some(inner) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        return match inner.parse::<Ipv6Addr>() {
            Ok(ip) if v6_is_public(ip) => Ok(()),
            _ => Err(UrlRejection::NotPublicHost),
        };
    }
    if let Ok(ip) = host.parse::<Ipv4Addr>() {
        return if v4_is_public(ip) {
            Ok(())
        } else {
            Err(UrlRejection::NotPublicHost)
        };
    }
    if domain_host_public(host) {
        Ok(())
    } else {
        Err(UrlRejection::NotPublicHost)
    }
}

#[cfg(test)]
mod tests {
    use super::UrlRejection;
    use super::validate_research_url;

    fn ok(u: &str) -> bool {
        validate_research_url(u).is_ok()
    }

    /// T27 — private/loopback/link-local/reserved research URLs are
    /// refused at input time (the system never fetches them anyway).
    #[test]
    fn t27_research_url_rejects_private_targets() {
        for u in [
            "http://127.0.0.1/",
            "http://127.1/",
            "http://2130706433/", // integer IP → 127.0.0.1
            "http://0x7f000001/", // hex IP → 127.0.0.1
            "http://10.0.0.5/",
            "http://192.168.1.1/",
            "http://172.16.0.1/",
            "http://169.254.169.254/latest/meta-data", // link-local/cloud meta
            "http://100.64.0.1/",                      // CGN
            "http://[::1]/",
            "http://[fe80::1]/",
            "http://[fc00::1]/",
            "http://0.0.0.0/",
            "http://224.0.0.1/",
            "http://198.51.100.1/", // documentation range
            "http://localhost/",
            "http://db.internal/",
            "http://intranet/",
            "ftp://example.com/x",
            "file:///etc/passwd",
            "http://user:pw@example.com/",
            "https://192.0.2.1/", // TEST-NET-1 documentation
            "not a url",
            "https://example.com@127.0.0.1/", // userinfo trick → 127.0.0.1 host
        ] {
            assert!(!ok(u), "expected rejection: {u}");
        }
        assert_eq!(
            validate_research_url("http://10.1.1.1"),
            Err(UrlRejection::NotPublicHost)
        );
        assert_eq!(
            validate_research_url("ftp://example.com"),
            Err(UrlRejection::Scheme)
        );
    }

    #[test]
    fn t27_public_urls_pass() {
        for u in [
            "https://example.com/about",
            "http://example.com",
            "https://sub.example.co.kr/path?q=1",
            "https://example.com:8443/x", // ports unrestricted — hosts are checked
            "https://8.8.8.8/dns",
            "https://[2606:4700:4700::1111]/",
        ] {
            assert!(ok(u), "expected accept: {u}");
        }
    }
}
