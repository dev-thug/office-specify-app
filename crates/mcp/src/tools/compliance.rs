//! compliance module MCP tools. Mirrors the authenticated REST handlers in
//! `crates/api/src/routes/compliance.rs` (communication bases and
//! suppressions). Public unsubscribe capability routes are not exposed.

use chrono::{DateTime, Utc};
use contracts::AppError;
use contracts::compliance::{
    CreateCommunicationBasisRequest, CreateSuppressionRequest, ReleaseSuppressionRequest,
    ReviewBasisRequest,
};
use infrastructure::repo::compliance::{BasisFilter, SuppressionFilter};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use super::operations::{
    PageOut, decode_keyset, encode_keyset, object_output, page_limit, page_output, schema,
};
use crate::{ToolDef, arguments, result, tool_with_db, tool_with_services};

const BASIS_DESCRIPTION: &str = "Communication basis: id, email_address_id, email_address, purpose, jurisdiction, basis_type, status, evidence_ref, collected_at, reviewed_by, reviewed_at, valid_until, revoked_at, version, created_at.";
const SUPPRESSION_DESCRIPTION: &str = "Suppression: id, email_address_id, email_address, purpose_scope, reason, status (active|released), source_outbound_id, created_by, released_at, released_by, release_evidence_ref, version, created_at.";

/// Tools this module exposes over MCP.
pub fn tools() -> Vec<ToolDef> {
    vec![
        tool_with_services(
            "compliance_list_bases",
            "List communication bases (consent/legal basis records per email address), newest first. Requires compliance read access. Optional filters: email_address_id, basis_purpose, basis_status, jurisdiction; follow next_cursor with identical filters.",
            schema::<ListBasesInput>(),
            page_output(BASIS_DESCRIPTION),
            false,
            |services, ctx, args| async move {
                let input = arguments::<ListBasesInput>(args)?;
                let limit = page_limit(input.limit)?;
                let filters = json!({
                    "kind": "communication-bases",
                    "email_address_id": input.email_address_id,
                    "purpose": input.basis_purpose.clone(),
                    "status": input.basis_status.clone(),
                    "jurisdiction": input.jurisdiction.clone(),
                });
                let after = decode_keyset(
                    &services.config.hmac_key,
                    &ctx,
                    input.cursor.as_deref(),
                    &filters,
                )?;
                let filter = BasisFilter {
                    email_address_id: input.email_address_id,
                    purpose: input.basis_purpose,
                    status: input.basis_status,
                    jurisdiction: input.jurisdiction,
                    after,
                };
                let (items, last) =
                    application::compliance::bases::list_bases(&services.db, &ctx, filter, limit)
                        .await?;
                result(PageOut {
                    items,
                    next_cursor: encode_keyset(&services.config.hmac_key, &ctx, &filters, last),
                    has_more: last.is_some(),
                })
            },
        ),
        tool_with_db(
            "compliance_create_basis",
            "Register a communication basis for an email address; it is always stored as pending until reviewed. Requires compliance write access. Inputs: email_address_id, basis_purpose, jurisdiction, basis_type, evidence_ref, collected_at, optional valid_until.",
            schema::<CreateBasisInput>(),
            object_output(BASIS_DESCRIPTION),
            false,
            |db, ctx, args| async move {
                let input = arguments::<CreateBasisInput>(args)?;
                let req = CreateCommunicationBasisRequest {
                    email_address_id: input.email_address_id,
                    purpose: input.basis_purpose,
                    jurisdiction: input.jurisdiction,
                    basis_type: input.basis_type,
                    evidence_ref: input.evidence_ref,
                    collected_at: input.collected_at,
                    valid_until: input.valid_until,
                };
                result(application::compliance::bases::create_basis(&db, &ctx, req).await?)
            },
        ),
        tool_with_db(
            "compliance_get_basis",
            "Read one communication basis by basis_id, including its current version for review/revoke. Requires compliance read access. No writes.",
            schema::<BasisIdInput>(),
            object_output(BASIS_DESCRIPTION),
            false,
            |db, ctx, args| async move {
                let input = arguments::<BasisIdInput>(args)?;
                result(application::compliance::bases::get_basis(&db, &ctx, input.basis_id).await?)
            },
        ),
        tool_with_db(
            "compliance_review_basis",
            "Decide a pending communication basis as valid or rejected (optionally with valid_until). Requires the compliance review role and the basis's current expected_version.",
            schema::<ReviewBasisInput>(),
            object_output(BASIS_DESCRIPTION),
            false,
            |db, ctx, args| async move {
                let input = arguments::<ReviewBasisInput>(args)?;
                let req = ReviewBasisRequest {
                    decision: input.decision,
                    valid_until: input.valid_until,
                };
                result(
                    application::compliance::bases::review_basis(
                        &db,
                        &ctx,
                        input.basis_id,
                        input.expected_version,
                        req,
                    )
                    .await?,
                )
            },
        ),
        tool_with_db(
            "compliance_revoke_basis",
            "Revoke a communication basis so it no longer authorizes sending. Requires the compliance review role and the basis's current expected_version.",
            schema::<RevokeBasisInput>(),
            object_output(BASIS_DESCRIPTION),
            false,
            |db, ctx, args| async move {
                let input = arguments::<RevokeBasisInput>(args)?;
                result(
                    application::compliance::bases::revoke_basis(
                        &db,
                        &ctx,
                        input.basis_id,
                        input.expected_version,
                    )
                    .await?,
                )
            },
        ),
        tool_with_services(
            "compliance_list_suppressions",
            "List suppressions (blocked email addresses), newest first. Requires compliance read access. Optional filters: email_address_id, purpose_scope, suppression_reason, suppression_status (active|released); follow next_cursor with identical filters.",
            schema::<ListSuppressionsInput>(),
            page_output(SUPPRESSION_DESCRIPTION),
            false,
            |services, ctx, args| async move {
                let input = arguments::<ListSuppressionsInput>(args)?;
                let limit = page_limit(input.limit)?;
                let released = match input.suppression_status.as_deref() {
                    None => None,
                    Some("active") => Some(false),
                    Some("released") => Some(true),
                    Some(_) => {
                        return Err(AppError::validation(
                            "suppression_status must be 'active' or 'released'",
                            vec![],
                        ));
                    }
                };
                let filters = json!({
                    "kind": "suppressions",
                    "email_address_id": input.email_address_id,
                    "purpose_scope": input.purpose_scope.clone(),
                    "reason": input.suppression_reason.clone(),
                    "status": input.suppression_status.clone(),
                });
                let after = decode_keyset(
                    &services.config.hmac_key,
                    &ctx,
                    input.cursor.as_deref(),
                    &filters,
                )?;
                let filter = SuppressionFilter {
                    email_address_id: input.email_address_id,
                    purpose_scope: input.purpose_scope,
                    reason: input.suppression_reason,
                    released,
                    after,
                };
                let (items, last) = application::compliance::suppressions::list_suppressions(
                    &services.db,
                    &ctx,
                    filter,
                    limit,
                )
                .await?;
                result(PageOut {
                    items,
                    next_cursor: encode_keyset(&services.config.hmac_key, &ctx, &filters, last),
                    has_more: last.is_some(),
                })
            },
        ),
        tool_with_services(
            "compliance_create_suppression",
            "Suppress an email address for marketing or all mail. Reaffirming an identical active suppression is idempotent (created=false). Requires compliance write access; the same per-agent add quota as REST applies.",
            schema::<CreateSuppressionInput>(),
            json!({
                "type": "object",
                "required": ["created", "suppression"],
                "properties": {
                    "created": {"type": "boolean"},
                    "suppression": {"type": "object", "description": SUPPRESSION_DESCRIPTION}
                }
            }),
            false,
            |services, ctx, args| async move {
                let input = arguments::<CreateSuppressionInput>(args)?;
                let req = CreateSuppressionRequest {
                    email_address_id: input.email_address_id,
                    purpose_scope: input.purpose_scope,
                    reason: input.suppression_reason,
                    source_outbound_id: input.source_outbound_id,
                };
                let out = application::compliance::suppressions::create_suppression(
                    &services.db,
                    &ctx,
                    services.rate_limiter.as_ref(),
                    req,
                )
                .await?;
                result(CreateSuppressionOutput {
                    created: out.created,
                    suppression: out.response,
                })
            },
        ),
        tool_with_db(
            "compliance_get_suppression",
            "Read one suppression by suppression_id. Requires compliance read access. No writes.",
            schema::<SuppressionIdInput>(),
            object_output(SUPPRESSION_DESCRIPTION),
            false,
            |db, ctx, args| async move {
                let input = arguments::<SuppressionIdInput>(args)?;
                result(
                    application::compliance::suppressions::get_suppression(
                        &db,
                        &ctx,
                        input.suppression_id,
                    )
                    .await?,
                )
            },
        ),
        tool_with_db(
            "compliance_request_suppression_release",
            "Open an approval request to release a suppression; it is never released directly. Requires the compliance review role, an evidence_ref and a release_reason.",
            schema::<RequestReleaseInput>(),
            object_output(
                "Release request: approval_request_id, suppression_id, status, expires_at.",
            ),
            false,
            |db, ctx, args| async move {
                let input = arguments::<RequestReleaseInput>(args)?;
                let req = ReleaseSuppressionRequest {
                    evidence_ref: input.evidence_ref,
                    reason: input.release_reason,
                };
                result(
                    application::compliance::suppressions::request_release(
                        &db,
                        &ctx,
                        input.suppression_id,
                        req,
                    )
                    .await?,
                )
            },
        ),
    ]
}

#[derive(Serialize)]
struct CreateSuppressionOutput {
    created: bool,
    suppression: contracts::compliance::SuppressionResponse,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListBasesInput {
    /// Optional email-address UUID (from crm_get_contact emails) to filter bases for.
    #[serde(default)]
    email_address_id: Option<Uuid>,
    /// Optional basis purpose filter: marketing, correspondence or transactional.
    #[serde(default)]
    basis_purpose: Option<String>,
    /// Optional basis status filter: pending, valid, rejected, revoked or expired.
    #[serde(default)]
    basis_status: Option<String>,
    /// Optional policy jurisdiction key filter, e.g. KR, US or default.
    #[serde(default)]
    jurisdiction: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    cursor: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateBasisInput {
    /// Email-address UUID (from crm_get_contact emails) the basis applies to; not a contact ID.
    email_address_id: Uuid,
    /// Basis purpose: marketing, correspondence or transactional.
    basis_purpose: String,
    /// Policy jurisdiction key, e.g. KR, US or default.
    jurisdiction: String,
    /// Basis type: explicit_consent, requested_reply, requested_transaction or reviewed_exception.
    basis_type: String,
    /// Pointer to the stored consent/exception evidence (document reference, URL or ticket).
    evidence_ref: String,
    /// RFC3339 timestamp when the consent/evidence was collected.
    collected_at: DateTime<Utc>,
    /// Optional RFC3339 timestamp after which the basis expires.
    #[serde(default)]
    valid_until: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct BasisIdInput {
    /// Communication basis UUID returned by compliance_list_bases or compliance_create_basis.
    basis_id: Uuid,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReviewBasisInput {
    /// Communication basis UUID returned by compliance_list_bases or compliance_create_basis.
    basis_id: Uuid,
    expected_version: i64,
    /// Review decision: `valid` or `rejected`.
    decision: String,
    /// Optional RFC3339 expiry for a `valid` decision; omit for `rejected`.
    #[serde(default)]
    valid_until: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RevokeBasisInput {
    /// Communication basis UUID returned by compliance_list_bases or compliance_create_basis.
    basis_id: Uuid,
    expected_version: i64,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListSuppressionsInput {
    /// Optional email-address UUID (from crm_get_contact emails) to filter suppressions for.
    #[serde(default)]
    email_address_id: Option<Uuid>,
    /// Optional suppression scope filter: marketing or all.
    #[serde(default)]
    purpose_scope: Option<String>,
    /// Optional suppression reason filter: unsubscribe, complaint, hard_bounce or manual.
    #[serde(default)]
    suppression_reason: Option<String>,
    /// Optional suppression status filter: active or released. Omit to list both.
    #[serde(default)]
    suppression_status: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    cursor: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateSuppressionInput {
    /// Email-address UUID (from crm_get_contact emails) to suppress; not a contact ID or literal address.
    email_address_id: Uuid,
    /// Suppression scope: `marketing` (blocks marketing mail only) or `all`.
    purpose_scope: String,
    /// Suppression reason: unsubscribe, complaint, hard_bounce or manual.
    suppression_reason: String,
    /// Optional UUID of the outbound message that triggered this suppression, when known.
    #[serde(default)]
    source_outbound_id: Option<Uuid>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SuppressionIdInput {
    /// Suppression UUID returned by compliance_list_suppressions or compliance_create_suppression.
    suppression_id: Uuid,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RequestReleaseInput {
    /// Suppression UUID returned by compliance_list_suppressions or compliance_create_suppression.
    suppression_id: Uuid,
    /// Required pointer to the evidence justifying the release (e.g. new consent record reference).
    evidence_ref: String,
    /// Required human-readable reason for releasing the suppression; shown to the approver.
    release_reason: String,
}
