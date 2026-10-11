//! `POST /lead-proposals/{id}/decisions` — accept or reject (B.3.12).
//!
//! Human + `research.write` only (§13.4 — agents propose, never approve).
//! Accept resolves the company (explicit target → normalized-domain
//! dedupe → create), inserts proposed contacts verbatim, stores candidate
//! emails with a `pending` communication basis (never sendable — T35),
//! and emits `lead.accepted` — all in one tx.

use contracts::research::{
    DecisionKind, LeadProposalDecisionRequest, LeadProposalDecisionResponse, LeadProposalStatus,
    ResearchTargetKind,
};
use contracts::{AppError, Ctx, ErrorCode, Scope, Violation};
use domain::email_address::{self, LocalPartPolicy};
use domain::research as d;
use infrastructure::audit::{self, AuditEntry};
use infrastructure::db::Db;
use infrastructure::outbox::{self, event_types};
use infrastructure::repo::research as repo;
use std::collections::BTreeMap;
use uuid::Uuid;

use crate::authz;

/// Communication-basis constants for researched email candidates: the
/// address was *seen*, never consented — a `pending` basis is the data-
/// level marker that keeps it out of every send path (T35).
const CANDIDATE_PURPOSE: &str = "marketing";
const CANDIDATE_JURISDICTION: &str = "unknown";
const CANDIDATE_BASIS: &str = "reviewed_exception";

pub async fn decide(
    db: &Db,
    ctx: &Ctx,
    proposal_id: Uuid,
    expected_version: i64,
    req: &LeadProposalDecisionRequest,
) -> Result<LeadProposalDecisionResponse, AppError> {
    authz::require_operator(ctx)?;
    authz::require_scope(ctx, Scope::ResearchWrite)?;

    let mut tx = db.begin(ctx.workspace_id).await?;
    let row = repo::lock_proposal(&mut tx, proposal_id)
        .await?
        .ok_or_else(|| AppError::not_found("lead proposal not found"))?;
    if !matches!(row.status.as_str(), "pending" | "needs_review") {
        return Err(AppError::conflict(format!(
            "proposal already {}",
            row.status
        )));
    }
    if row.version != expected_version {
        return Err(AppError::new(
            ErrorCode::VersionMismatch,
            "proposal version does not match If-Match",
        ));
    }

    let res = match req.decision {
        DecisionKind::Reject => decide_reject(&mut tx, ctx, &row, req).await?,
        DecisionKind::Accept => decide_accept(&mut tx, ctx, &row, req).await?,
    };
    tx.commit().await?;
    Ok(res)
}

async fn decide_reject(
    tx: &mut infrastructure::db::Tx<'_>,
    ctx: &Ctx,
    row: &repo::LeadProposalRow,
    req: &LeadProposalDecisionRequest,
) -> Result<LeadProposalDecisionResponse, AppError> {
    let version =
        repo::decide_proposal(tx, row.id, row.version, "rejected", None, ctx.principal_id)
            .await?
            .ok_or_else(|| AppError::new(ErrorCode::VersionMismatch, "proposal changed"))?;
    let mut entry = AuditEntry::new(
        ctx.principal_id,
        "lead.rejected",
        "lead_proposal",
        ctx.request_id.clone(),
    )
    .with_target(row.id);
    entry.metadata = audit::details(&serde_json::json!({
        "reason": req.reason,
    }));
    audit::record(tx, entry).await?;
    Ok(LeadProposalDecisionResponse {
        id: row.id,
        status: LeadProposalStatus::Rejected,
        accepted_company_id: None,
        contact_ids: Vec::new(),
        email_address_ids: Vec::new(),
        version,
    })
}

async fn decide_accept(
    tx: &mut infrastructure::db::Tx<'_>,
    ctx: &Ctx,
    row: &repo::LeadProposalRow,
    req: &LeadProposalDecisionRequest,
) -> Result<LeadProposalDecisionResponse, AppError> {
    if row
        .proposed_data
        .get("proposal_kind")
        .and_then(|v| v.as_str())
        == Some("crm_update")
    {
        return decide_accept_update(tx, ctx, row, req).await;
    }

    // --- company resolution: explicit target → domain dedupe → create ---
    let domain_norm = row.domain.as_deref().and_then(d::normalize_domain);
    let company_id = if let Some(explicit) = req.company_id {
        if !repo::company_exists(tx, explicit).await? {
            return Err(
                AppError::new(ErrorCode::ValidationFailed, "company_id not found")
                    .with_status(422)
                    .with_violations(vec![Violation {
                        field: "company_id".into(),
                        code: "not_found".into(),
                    }]),
            );
        }
        explicit
    } else if let Some(dom) = domain_norm.as_deref()
        && let Some(existing) = repo::company_id_by_domain(tx, dom).await?
    {
        existing
    } else {
        let f = d::extract_company_fields(&row.proposed_data);
        repo::insert_company(
            tx,
            row.company_name.trim(),
            domain_norm.as_deref().or(row.domain.as_deref()),
            f.industry.as_deref(),
            f.country_code.as_deref(),
            f.website_url.as_deref(),
        )
        .await?
    };

    // --- contacts + candidate emails (verbatim, never fabricated) ---
    let mut contact_ids = Vec::new();
    let mut email_address_ids = Vec::new();
    for c in d::extract_contacts(&row.proposed_data) {
        if c.name.chars().count() > d::MAX_FIELD_LEN {
            continue; // belt + suspenders — batch validation already bounded
        }
        let contact_id = repo::insert_contact(
            tx,
            Some(company_id),
            &c.name,
            c.job_title.as_deref(),
            c.department.as_deref(),
            Some(ctx.principal_id),
        )
        .await?;
        contact_ids.push(contact_id);
        if let Some(candidate) = &c.email
            && let Ok(n) = email_address::normalize(candidate, LocalPartPolicy::Lowercase)
        {
            // Unparseable candidates are skipped — never "fixed" into a
            // different address (§13.4 never fabricate).
            let addr_id = repo::find_or_create_email_address(
                tx,
                &n.original,
                &n.normalized_key,
                n.normalization_version,
            )
            .await?;
            repo::link_contact_email(tx, contact_id, addr_id, None, true).await?;
            if !repo::basis_exists(tx, addr_id, CANDIDATE_PURPOSE, CANDIDATE_JURISDICTION).await? {
                repo::insert_pending_basis(
                    tx,
                    addr_id,
                    CANDIDATE_PURPOSE,
                    CANDIDATE_JURISDICTION,
                    CANDIDATE_BASIS,
                    &format!("lead-proposal:{}", row.id),
                )
                .await?;
            }
            email_address_ids.push(addr_id);
        }
    }

    let version = repo::decide_proposal(
        tx,
        row.id,
        row.version,
        "accepted",
        Some(company_id),
        ctx.principal_id,
    )
    .await?
    .ok_or_else(|| AppError::new(ErrorCode::VersionMismatch, "proposal changed"))?;

    outbox::emit(
        tx,
        event_types::LEAD_ACCEPTED,
        "lead_proposal",
        row.id,
        version,
        serde_json::json!({
            "lead_proposal_id": row.id,
            "company_id": company_id,
            "contact_id": contact_ids.first().copied(),
        }),
    )
    .await?;

    let mut entry = AuditEntry::new(
        ctx.principal_id,
        "lead.accepted",
        "lead_proposal",
        ctx.request_id.clone(),
    )
    .with_target(row.id);
    entry.metadata = audit::details(&serde_json::json!({
        "company_id": company_id,
        "contact_ids": contact_ids,
        "reason": req.reason,
    }));
    audit::record(tx, entry).await?;

    Ok(LeadProposalDecisionResponse {
        id: row.id,
        status: LeadProposalStatus::Accepted,
        accepted_company_id: Some(company_id),
        contact_ids,
        email_address_ids,
        version,
    })
}

/// Accept an evidence-backed CRM update proposal. Revalidate the stored
/// before/after diff and the current target version before applying any
/// change; every write, audit row, and outbox event shares the decision tx.
async fn decide_accept_update(
    tx: &mut infrastructure::db::Tx<'_>,
    ctx: &Ctx,
    row: &repo::LeadProposalRow,
    req: &LeadProposalDecisionRequest,
) -> Result<LeadProposalDecisionResponse, AppError> {
    if req.company_id.is_some() {
        return Err(AppError::validation(
            "company_id cannot override an update proposal target",
            vec![Violation {
                field: "company_id".into(),
                code: "not_allowed_for_update".into(),
            }],
        )
        .with_status(422));
    }
    let invalid = || {
        AppError::validation(
            "stored CRM update proposal is invalid",
            vec![Violation {
                field: "proposed_data".into(),
                code: "invalid_update_proposal".into(),
            }],
        )
        .with_status(422)
    };
    let target = row
        .proposed_data
        .get("target")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(invalid)?;
    let kind = match target.get("kind").and_then(serde_json::Value::as_str) {
        Some("company") => ResearchTargetKind::Company,
        Some("contact") => ResearchTargetKind::Contact,
        _ => return Err(invalid()),
    };
    let target_id = target
        .get("id")
        .and_then(serde_json::Value::as_str)
        .and_then(|id| id.parse::<Uuid>().ok())
        .ok_or_else(invalid)?;
    let expected_version = row
        .proposed_data
        .get("expected_version")
        .and_then(serde_json::Value::as_i64)
        .filter(|v| *v > 0)
        .ok_or_else(invalid)?;
    let stored_diff = row
        .proposed_data
        .get("field_changes")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(invalid)?;
    let mut requested_after = BTreeMap::new();
    let mut requested_before = BTreeMap::new();
    for (field, diff) in stored_diff {
        let change = diff.as_object().ok_or_else(invalid)?;
        requested_before.insert(
            field.clone(),
            change.get("before").cloned().ok_or_else(invalid)?,
        );
        requested_after.insert(
            field.clone(),
            change.get("after").cloned().ok_or_else(invalid)?,
        );
    }
    let (normalized, violations) =
        super::update_proposals::normalize_changes(kind, &requested_after);
    if !violations.is_empty() || normalized.is_empty() || normalized != requested_after {
        return Err(invalid());
    }
    let evidence = repo::list_evidence_for_proposal(tx, row.id).await?;
    if evidence.is_empty()
        || evidence.iter().any(|ev| ev.review_status == "rejected")
        || requested_after
            .keys()
            .any(|field| !evidence.iter().any(|ev| ev.field_path == *field))
    {
        return Err(invalid());
    }

    let target_row = repo::lock_update_target(tx, kind, target_id)
        .await?
        .ok_or_else(|| AppError::not_found("CRM update target not found"))?;
    if target_row.version != expected_version {
        return Err(AppError::new(
            ErrorCode::VersionMismatch,
            "CRM update target changed since the proposal was created",
        ));
    }
    for (field, before) in &requested_before {
        if target_row
            .fields
            .get(field)
            .unwrap_or(&serde_json::Value::Null)
            != before
        {
            return Err(AppError::new(
                ErrorCode::SnapshotMismatch,
                "CRM update proposal before-values no longer match the target",
            ));
        }
    }

    let after = serde_json::Value::Object(normalized.into_iter().collect());
    let target_version = repo::apply_update_target(tx, kind, target_id, expected_version, &after)
        .await?
        .ok_or_else(|| {
            AppError::new(
                ErrorCode::VersionMismatch,
                "CRM update target changed during acceptance",
            )
        })?;
    let company_id = target_row.company_id;
    let proposal_version = repo::decide_proposal(
        tx,
        row.id,
        row.version,
        "accepted",
        company_id,
        ctx.principal_id,
    )
    .await?
    .ok_or_else(|| AppError::new(ErrorCode::VersionMismatch, "proposal changed"))?;
    let contact_id = (kind == ResearchTargetKind::Contact).then_some(target_id);
    outbox::emit(
        tx,
        event_types::LEAD_ACCEPTED,
        "lead_proposal",
        row.id,
        proposal_version,
        serde_json::json!({
            "lead_proposal_id": row.id,
            "company_id": company_id,
            "contact_id": contact_id,
        }),
    )
    .await?;

    let mut entry = AuditEntry::new(
        ctx.principal_id,
        "crm.update_accepted",
        "lead_proposal",
        ctx.request_id.clone(),
    )
    .with_target(row.id);
    entry.metadata = audit::details(&serde_json::json!({
        "target_kind": kind.as_str(),
        "target_id": target_id,
        "target_version": target_version,
        "fields": requested_after.keys().collect::<Vec<_>>(),
        "reason": req.reason,
    }));
    audit::record(tx, entry).await?;

    Ok(LeadProposalDecisionResponse {
        id: row.id,
        status: LeadProposalStatus::Accepted,
        accepted_company_id: company_id,
        contact_ids: contact_id.into_iter().collect(),
        email_address_ids: Vec::new(),
        version: proposal_version,
    })
}
