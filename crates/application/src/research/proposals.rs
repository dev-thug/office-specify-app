//! `POST /lead-proposals` (batch, all-or-nothing), list + get (B.3.12).

use chrono::{DateTime, Utc};
use contracts::research::{
    CreateLeadProposalsRequest, CreateLeadProposalsResponse, EvidenceResponse,
    LeadProposalDetailResponse, LeadProposalResponse, LeadProposalResultItem, LeadProposalStatus,
};
use contracts::{AppError, Ctx, ErrorCode, Scope, Violation};
use domain::research as d;
use infrastructure::audit::{self, AuditEntry};
use infrastructure::db::{Db, Tx};
use infrastructure::outbox;
use infrastructure::repo::research as repo;
use uuid::Uuid;

use crate::authz;

/// Readers of proposals: CRM readers or research participants.
fn require_read(ctx: &Ctx) -> Result<(), AppError> {
    for s in [Scope::CrmRead, Scope::ResearchPropose, Scope::ResearchWrite] {
        if ctx.scopes.contains(s) {
            return Ok(());
        }
    }
    Err(AppError::new(
        ErrorCode::ScopeRequired,
        "missing scope crm.read or research.propose",
    ))
}

/// Batch create — all-or-nothing (B.3.12): every item is validated up
/// front and the whole request 422s on any violation. Inside the caller's
/// idempotency tx.
pub async fn create_proposals_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    req: &CreateLeadProposalsRequest,
) -> Result<CreateLeadProposalsResponse, AppError> {
    authz::require_scope(ctx, Scope::ResearchPropose)?;

    let mut violations = Vec::new();
    if req.proposals.is_empty() {
        violations.push(Violation {
            field: "proposals".into(),
            code: "required".into(),
        });
    }
    if req.proposals.len() > d::MAX_BATCH_PROPOSALS {
        violations.push(Violation {
            field: "proposals".into(),
            code: format!("max_{}", d::MAX_BATCH_PROPOSALS),
        });
    }
    for (i, item) in req.proposals.iter().enumerate() {
        violations.extend(d::validate_proposal(item, i));
        // Deep URL policy per evidence source (§15.1/§13.5 SSRF rules).
        for (j, ev) in item.evidence.iter().enumerate() {
            if let Err(reason) = repo::validate_research_url(&ev.source_url) {
                violations.push(Violation {
                    field: format!("proposals[{i}].evidence[{j}].source_url"),
                    code: format!("invalid_url:{reason:?}").to_lowercase(),
                });
            }
        }
    }
    // agent_run must exist in this workspace; an agent may only attach a
    // run it owns.
    if let Some(run_id) = req.agent_run_id {
        match repo::agent_run_principal(tx, run_id).await? {
            None => violations.push(Violation {
                field: "agent_run_id".into(),
                code: "not_found".into(),
            }),
            Some(owner)
                if ctx.principal_kind == contracts::PrincipalKind::Agent
                    && owner != ctx.principal_id =>
            {
                violations.push(Violation {
                    field: "agent_run_id".into(),
                    code: "not_owned".into(),
                });
            }
            _ => {}
        }
    }
    if !violations.is_empty() {
        return Err(
            AppError::new(ErrorCode::ValidationFailed, "invalid proposal batch")
                .with_status(422)
                .with_violations(violations),
        );
    }

    let mut items = Vec::with_capacity(req.proposals.len());
    for item in &req.proposals {
        let domain_norm = item.domain.as_deref().and_then(d::normalize_domain);
        let proposal_id = repo::insert_proposal(
            tx,
            req.agent_run_id,
            ctx.principal_id,
            item.external_key.as_deref(),
            item.company_name.trim(),
            domain_norm.as_deref().or(item.domain.as_deref()),
            &item.proposed_data,
        )
        .await?;
        for ev in &item.evidence {
            let evidence_id = repo::insert_evidence(
                tx,
                repo::EvidenceTarget::LeadProposal(proposal_id),
                ev.field_path.trim(),
                ev.source_url.trim(),
                ev.observed_at,
                ev.excerpt.as_deref(),
                &ev.value,
                ev.evidence_type.as_str(),
                ev.confidence,
                ctx.principal_id,
            )
            .await?;
            outbox::emit(
                tx,
                "research.evidence_created",
                "research_evidence",
                evidence_id,
                1,
                serde_json::json!({
                    "research_evidence_id": evidence_id,
                    "lead_proposal_id": proposal_id,
                }),
            )
            .await?;
        }
        let duplicate_company_ids =
            repo::find_duplicate_company_ids(tx, domain_norm.as_deref(), item.company_name.trim())
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
        audit::record(
            tx,
            AuditEntry::new(
                ctx.principal_id,
                "lead.proposed",
                "lead_proposal",
                ctx.request_id.clone(),
            )
            .with_target(proposal_id),
        )
        .await?;
        items.push(LeadProposalResultItem {
            id: proposal_id,
            status: LeadProposalStatus::Pending,
            duplicate_company_ids,
            warnings: Vec::new(),
        });
    }
    Ok(CreateLeadProposalsResponse { proposals: items })
}

/// Non-idempotent path (no Idempotency-Key header): own tx.
pub async fn create_proposals(
    db: &Db,
    ctx: &Ctx,
    req: &CreateLeadProposalsRequest,
) -> Result<CreateLeadProposalsResponse, AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    let res = create_proposals_tx(&mut tx, ctx, req).await?;
    tx.commit().await?;
    Ok(res)
}

fn proposal_response(r: repo::LeadProposalRow, dups: Vec<Uuid>) -> LeadProposalResponse {
    LeadProposalResponse {
        id: r.id,
        agent_run_id: r.agent_run_id,
        proposed_by: r.proposed_by,
        external_key: r.external_key,
        company_name: r.company_name,
        domain: r.domain,
        proposed_data: r.proposed_data,
        status: LeadProposalStatus::parse(&r.status).unwrap_or(LeadProposalStatus::Pending),
        accepted_company_id: r.accepted_company_id,
        reviewed_by: r.reviewed_by,
        reviewed_at: r.reviewed_at,
        duplicate_company_ids: dups,
        version: r.version,
        created_at: r.created_at,
    }
}

pub fn evidence_response(r: &repo::EvidenceRow) -> EvidenceResponse {
    use contracts::research::{EvidenceType, ReviewStatus};
    EvidenceResponse {
        id: r.id,
        company_id: r.company_id,
        contact_id: r.contact_id,
        lead_proposal_id: r.lead_proposal_id,
        field_path: r.field_path.clone(),
        source_url: r.source_url.clone(),
        observed_at: r.observed_at,
        excerpt: r.excerpt.clone(),
        value: r.value.clone(),
        evidence_type: match r.evidence_type.as_str() {
            "inferred" => EvidenceType::Inferred,
            _ => EvidenceType::Observed,
        },
        confidence: r.confidence,
        review_status: match r.review_status.as_str() {
            "verified" => ReviewStatus::Verified,
            "rejected" => ReviewStatus::Rejected,
            _ => ReviewStatus::Pending,
        },
        created_by: r.created_by,
        reviewed_by: r.reviewed_by,
        created_at: r.created_at,
    }
}

/// `GET /lead-proposals` — keyset over (created_at, id), newest first.
/// Returns rows + the last key for the next cursor.
pub async fn list_proposals(
    db: &Db,
    ctx: &Ctx,
    status: Option<&str>,
    after: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
) -> Result<(Vec<LeadProposalResponse>, Option<(DateTime<Utc>, Uuid)>), AppError> {
    require_read(ctx)?;
    if let Some(s) = status
        && LeadProposalStatus::parse(s).is_none()
    {
        return Err(AppError::validation(
            "unknown status filter",
            vec![Violation {
                field: "status".into(),
                code: "invalid".into(),
            }],
        ));
    }
    let mut tx = db.begin(ctx.workspace_id).await?;
    let rows = repo::list_proposals(&mut tx, status, after, limit + 1).await?;
    let has_more = rows.len() as i64 > limit;
    let rows: Vec<_> = rows.into_iter().take(limit as usize).collect();
    let next = if has_more {
        rows.last().map(|r| (r.created_at, r.id))
    } else {
        None
    };
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let dups = repo::find_duplicate_company_ids(
            &mut tx,
            r.domain.as_deref().and_then(d::normalize_domain).as_deref(),
            &r.company_name,
        )
        .await?;
        out.push(proposal_response(r, dups));
    }
    tx.commit().await?;
    Ok((out, next))
}

/// `GET /lead-proposals/{id}` — proposal + attached evidence.
pub async fn get_proposal(
    db: &Db,
    ctx: &Ctx,
    id: Uuid,
) -> Result<LeadProposalDetailResponse, AppError> {
    require_read(ctx)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let row = repo::get_proposal(&mut tx, id)
        .await?
        .ok_or_else(|| AppError::not_found("lead proposal not found"))?;
    let evidence = repo::list_evidence_for_proposal(&mut tx, id).await?;
    let dups = repo::find_duplicate_company_ids(
        &mut tx,
        row.domain
            .as_deref()
            .and_then(d::normalize_domain)
            .as_deref(),
        &row.company_name,
    )
    .await?;
    let res = LeadProposalDetailResponse {
        proposal: proposal_response(row, dups),
        evidence: evidence.iter().map(evidence_response).collect(),
    };
    tx.commit().await?;
    Ok(res)
}
