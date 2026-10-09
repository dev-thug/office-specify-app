//! Agent proposals to change existing CRM fields (MCP `crm_propose_update`).
//!
//! The target row is locked and compared with `expected_version`; this use
//! case stores an evidence-backed before/after proposal and deliberately
//! leaves the company/contact row unchanged until a human decision.

use crate::authz;
use contracts::research::{
    EvidenceInput, LeadProposalResponse, LeadProposalStatus, ProposeUpdateRequest,
    ResearchTargetKind,
};
use contracts::{AppError, Ctx, ErrorCode, Scope, Violation};
use domain::research as research_domain;
use infrastructure::audit::{self, AuditEntry};
use infrastructure::crypto;
use infrastructure::db::{Db, Tx};
use infrastructure::idempotency::{self, Scope as IdempotencyScope, Stored};
use infrastructure::outbox;
use infrastructure::repo::research as repo;

const MAX_FIELDS: usize = 20;
const MAX_EVIDENCE: usize = 40;

/// Create or replay a pending CRM update proposal. MCP mutations provide
/// their idempotency key in the tool arguments, so this is the canonical
/// boundary for that key and the full request hash.
pub async fn propose_update(
    db: &Db,
    ctx: &Ctx,
    req: &ProposeUpdateRequest,
) -> Result<LeadProposalResponse, AppError> {
    if !(16..=128).contains(&req.idempotency_key.len()) {
        return Err(AppError::validation(
            "idempotency_key must contain 16..=128 characters",
            vec![],
        ));
    }
    let scope = IdempotencyScope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: "MCP crm_propose_update".into(),
        key: req.idempotency_key.clone(),
    };
    let body = serde_json::to_vec(req)
        .map_err(|e| AppError::internal(format!("update proposal serialize: {e}")))?;
    let request_hash = crypto::sha256_bytes(&body);
    let ctx = ctx.clone();
    let req = req.clone();
    let outcome = idempotency::run(db, &scope, &request_hash, move |mut tx| async move {
        let response = create_update_proposal_tx(&mut tx, &ctx, &req).await?;
        Ok((
            tx,
            Stored {
                status: 201,
                body: response,
            },
        ))
    })
    .await?;
    Ok(outcome.into_inner().body)
}

/// Transaction-level implementation, also used by decision-side tests and
/// any future REST adapter. It never mutates the target CRM row.
pub async fn create_update_proposal_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    req: &ProposeUpdateRequest,
) -> Result<LeadProposalResponse, AppError> {
    authz::require_scope(ctx, Scope::ResearchPropose)?;
    let mut violations = validate_request_shape(req);

    match repo::agent_run_principal(tx, req.agent_run_id).await? {
        None => violations.push(Violation {
            field: "agent_run_id".into(),
            code: "not_found".into(),
        }),
        Some(owner) if owner != ctx.principal_id => {
            return Err(AppError::forbidden(
                "the research run does not belong to this principal",
            ));
        }
        Some(_) => {}
    }

    if !violations.is_empty() {
        return Err(invalid_update(violations));
    }

    let target = repo::lock_update_target(tx, req.target.kind, req.target.id)
        .await?
        .ok_or_else(|| AppError::not_found("CRM update target not found"))?;
    if target.version != req.expected_version {
        return Err(AppError::new(
            ErrorCode::VersionMismatch,
            "target version does not match expected_version",
        ));
    }

    let (normalized_changes, mut value_violations) =
        normalize_changes(req.target.kind, &req.field_changes);
    let changed_fields: Vec<_> = normalized_changes.keys().cloned().collect();
    for field in &changed_fields {
        if !req.evidence.iter().any(|ev| ev.field_path == *field) {
            value_violations.push(Violation {
                field: format!("evidence.{field}"),
                code: "required_for_changed_field".into(),
            });
        }
    }
    for (i, evidence) in req.evidence.iter().enumerate() {
        value_violations.extend(validate_evidence(evidence, i));
    }
    if !value_violations.is_empty() {
        return Err(invalid_update(value_violations));
    }

    let mut diff = serde_json::Map::new();
    for (field, after) in normalized_changes {
        let before = target
            .fields
            .get(&field)
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        if before == after {
            value_violations.push(Violation {
                field: format!("field_changes.{field}"),
                code: "no_change".into(),
            });
            continue;
        }
        diff.insert(field, serde_json::json!({"before": before, "after": after}));
    }
    if !value_violations.is_empty() {
        return Err(invalid_update(value_violations));
    }
    let proposed_data = serde_json::json!({
        "proposal_kind": "crm_update",
        "target": {"kind": req.target.kind.as_str(), "id": req.target.id},
        "company_id": target.company_id,
        "expected_version": req.expected_version,
        "field_changes": diff,
    });
    let proposal_id = repo::insert_proposal(
        tx,
        Some(req.agent_run_id),
        ctx.principal_id,
        Some(&req.idempotency_key),
        &target.label,
        target.domain.as_deref(),
        &proposed_data,
    )
    .await?;
    outbox::emit(
        tx,
        "lead.proposed",
        "lead_proposal",
        proposal_id,
        1,
        serde_json::json!({"lead_proposal_id": proposal_id}),
    )
    .await?;
    for evidence in &req.evidence {
        repo::insert_evidence(
            tx,
            repo::EvidenceTarget::LeadProposal(proposal_id),
            evidence.field_path.trim(),
            evidence.source_url.trim(),
            evidence.observed_at,
            evidence.excerpt.as_deref(),
            &evidence.value,
            evidence.evidence_type.as_str(),
            evidence.confidence,
            ctx.principal_id,
        )
        .await?;
    }
    let mut audit = AuditEntry::new(
        ctx.principal_id,
        "crm.update_proposed",
        "lead_proposal",
        ctx.request_id.clone(),
    )
    .with_target(proposal_id);
    audit.metadata = audit::details(&serde_json::json!({
        "target_kind": req.target.kind.as_str(),
        "target_id": req.target.id,
        "expected_version": req.expected_version,
        "fields": changed_fields,
        "evidence_count": req.evidence.len(),
    }));
    audit::record(tx, audit).await?;

    let row = repo::get_proposal(tx, proposal_id)
        .await?
        .ok_or_else(|| AppError::internal("CRM update proposal insert lost"))?;
    Ok(LeadProposalResponse {
        id: row.id,
        agent_run_id: row.agent_run_id,
        proposed_by: row.proposed_by,
        external_key: row.external_key,
        company_name: row.company_name,
        domain: row.domain,
        proposed_data: row.proposed_data,
        status: LeadProposalStatus::Pending,
        accepted_company_id: None,
        reviewed_by: None,
        reviewed_at: None,
        duplicate_company_ids: Vec::new(),
        version: row.version,
        created_at: row.created_at,
    })
}

fn validate_request_shape(req: &ProposeUpdateRequest) -> Vec<Violation> {
    let mut violations = Vec::new();
    if !(16..=128).contains(&req.idempotency_key.len()) {
        violations.push(Violation {
            field: "idempotency_key".into(),
            code: "length_16_to_128_required".into(),
        });
    }
    if req.expected_version <= 0 {
        violations.push(Violation {
            field: "expected_version".into(),
            code: "must_be_positive".into(),
        });
    }
    if req.field_changes.is_empty() {
        violations.push(Violation {
            field: "field_changes".into(),
            code: "required".into(),
        });
    } else if req.field_changes.len() > MAX_FIELDS {
        violations.push(Violation {
            field: "field_changes".into(),
            code: format!("max_{MAX_FIELDS}"),
        });
    }
    if req.evidence.is_empty() {
        violations.push(Violation {
            field: "evidence".into(),
            code: "required".into(),
        });
    } else if req.evidence.len() > MAX_EVIDENCE {
        violations.push(Violation {
            field: "evidence".into(),
            code: format!("max_{MAX_EVIDENCE}"),
        });
    }
    violations
}

fn validate_evidence(evidence: &EvidenceInput, index: usize) -> Vec<Violation> {
    let mut violations = Vec::new();
    let prefix = format!("evidence[{index}]");
    if evidence.field_path.trim().is_empty() || evidence.field_path.len() > 100 {
        violations.push(Violation {
            field: format!("{prefix}.field_path"),
            code: "required_or_too_long".into(),
        });
    }
    if let Err(reason) = repo::validate_research_url(&evidence.source_url) {
        violations.push(Violation {
            field: format!("{prefix}.source_url"),
            code: format!("invalid_url:{reason:?}").to_lowercase(),
        });
    }
    if evidence.excerpt.as_ref().is_some_and(|x| x.len() > 2000) {
        violations.push(Violation {
            field: format!("{prefix}.excerpt"),
            code: "too_long".into(),
        });
    }
    if evidence
        .confidence
        .is_some_and(|x| !(0.0..=1.0).contains(&x))
    {
        violations.push(Violation {
            field: format!("{prefix}.confidence"),
            code: "out_of_range".into(),
        });
    }
    violations
}

pub(crate) fn normalize_changes(
    kind: ResearchTargetKind,
    changes: &std::collections::BTreeMap<String, serde_json::Value>,
) -> (
    std::collections::BTreeMap<String, serde_json::Value>,
    Vec<Violation>,
) {
    let mut normalized = std::collections::BTreeMap::new();
    let mut violations = Vec::new();
    for (field, value) in changes {
        let is_company_field = matches!(
            field.as_str(),
            "name"
                | "domain"
                | "website_url"
                | "industry"
                | "country_code"
                | "lifecycle_stage"
                | "description"
        );
        let is_contact_field = matches!(
            field.as_str(),
            "name" | "job_title" | "department" | "status"
        );
        if !(match kind {
            ResearchTargetKind::Company => is_company_field,
            ResearchTargetKind::Contact => is_contact_field,
        }) {
            violations.push(Violation {
                field: format!("field_changes.{field}"),
                code: "field_not_allowlisted".into(),
            });
            continue;
        }
        match normalize_value(kind, field, value) {
            Ok(value) => {
                normalized.insert(field.clone(), value);
            }
            Err(code) => violations.push(Violation {
                field: format!("field_changes.{field}"),
                code,
            }),
        }
    }
    (normalized, violations)
}

fn normalize_value(
    kind: ResearchTargetKind,
    field: &str,
    value: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    if value.is_null() {
        if field == "name" || field == "lifecycle_stage" || field == "status" {
            return Err("cannot_be_null".into());
        }
        return Ok(serde_json::Value::Null);
    }
    let Some(raw) = value.as_str() else {
        return Err("string_required".into());
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return if matches!(field, "name" | "lifecycle_stage" | "status") {
            Err("required".into())
        } else {
            Ok(serde_json::Value::Null)
        };
    }
    if trimmed.chars().count() > 2000 {
        return Err("too_long".into());
    }
    let normalized = match field {
        "name" if trimmed.chars().count() > 300 => return Err("too_long".into()),
        "domain" => research_domain::normalize_domain(trimmed)
            .ok_or_else(|| "invalid_domain".to_string())?,
        "website_url" => {
            repo::validate_research_url(trimmed).map_err(|_| "invalid_url".to_string())?;
            trimmed.to_string()
        }
        "country_code" => {
            let country = trimmed.to_ascii_uppercase();
            if country.len() != 2 || !country.bytes().all(|b| b.is_ascii_uppercase()) {
                return Err("invalid_country_code".into());
            }
            country
        }
        "lifecycle_stage"
            if !["prospect", "engaged", "qualified", "customer"].contains(&trimmed) =>
        {
            return Err("invalid_lifecycle_stage".into());
        }
        "status"
            if kind == ResearchTargetKind::Contact
                && !["new", "active", "departed", "inactive"].contains(&trimmed) =>
        {
            return Err("invalid_contact_status".into());
        }
        _ => trimmed.to_string(),
    };
    Ok(serde_json::Value::String(normalized))
}

fn invalid_update(violations: Vec<Violation>) -> AppError {
    AppError::new(ErrorCode::ValidationFailed, "invalid CRM update proposal")
        .with_status(422)
        .with_violations(violations)
}
