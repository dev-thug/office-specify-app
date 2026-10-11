//! `/research-evidence` — create (agents + humans), list/get, human review
//! (B.3.12 evidence rows).

use chrono::{DateTime, Utc};
use contracts::research::{
    CreateEvidenceRequest, EvidenceResponse, EvidenceReviewDecision, EvidenceReviewRequest,
};
use contracts::{AppError, Ctx, ErrorCode, Scope, Violation};
use infrastructure::audit::{self, AuditEntry};
use infrastructure::db::{Db, Tx};
use infrastructure::outbox;
use infrastructure::repo::research as repo;
use uuid::Uuid;

use super::proposals::evidence_response;
use crate::authz;

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

/// `POST /research-evidence` — inside the caller's idempotency tx.
pub async fn create_evidence_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    req: &CreateEvidenceRequest,
) -> Result<EvidenceResponse, AppError> {
    authz::require_scope(ctx, Scope::ResearchWrite)?;
    create_evidence_inner_tx(tx, ctx, req).await
}

async fn create_evidence_inner_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    req: &CreateEvidenceRequest,
) -> Result<EvidenceResponse, AppError> {
    // Exactly one target (DDL CHECK mirrored as a clean 422).
    let targets = [req.company_id, req.contact_id, req.lead_proposal_id]
        .iter()
        .flatten()
        .count();
    let mut violations = Vec::new();
    if targets != 1 {
        violations.push(Violation {
            field: "company_id|contact_id|lead_proposal_id".into(),
            code: "exactly_one_required".into(),
        });
    }
    if req.field_path.trim().is_empty() {
        violations.push(Violation {
            field: "field_path".into(),
            code: "required".into(),
        });
    }
    if let Err(reason) = repo::validate_research_url(&req.source_url) {
        violations.push(Violation {
            field: "source_url".into(),
            code: format!("invalid_url:{reason:?}").to_lowercase(),
        });
    }
    if let Some(c) = req.confidence
        && !(0.0..=1.0).contains(&c)
    {
        violations.push(Violation {
            field: "confidence".into(),
            code: "out_of_range".into(),
        });
    }
    // Target existence (workspace-scoped).
    if violations.is_empty() {
        let exists = if let Some(id) = req.company_id {
            repo::company_exists(tx, id).await?
        } else if let Some(id) = req.contact_id {
            repo::contact_exists(tx, id).await?
        } else if let Some(id) = req.lead_proposal_id {
            repo::get_proposal(tx, id).await?.is_some()
        } else {
            false
        };
        if !exists {
            violations.push(Violation {
                field: "target".into(),
                code: "not_found".into(),
            });
        }
    }
    if !violations.is_empty() {
        return Err(
            AppError::new(ErrorCode::ValidationFailed, "invalid evidence")
                .with_status(422)
                .with_violations(violations),
        );
    }

    let target = if let Some(id) = req.company_id {
        repo::EvidenceTarget::Company(id)
    } else if let Some(id) = req.contact_id {
        repo::EvidenceTarget::Contact(id)
    } else {
        repo::EvidenceTarget::LeadProposal(req.lead_proposal_id.unwrap_or_default())
    };
    let id = repo::insert_evidence(
        tx,
        target,
        req.field_path.trim(),
        req.source_url.trim(),
        req.observed_at,
        req.excerpt.as_deref(),
        &req.value,
        req.evidence_type.as_str(),
        req.confidence,
        ctx.principal_id,
    )
    .await?;
    outbox::emit(
        tx,
        "research.evidence_created",
        "research_evidence",
        id,
        1,
        serde_json::json!({
            "research_evidence_id": id,
            "lead_proposal_id": req.lead_proposal_id,
        }),
    )
    .await?;
    audit::record(
        tx,
        AuditEntry::new(
            ctx.principal_id,
            "evidence.created",
            "research_evidence",
            ctx.request_id.clone(),
        )
        .with_target(id),
    )
    .await?;
    let row = repo::get_evidence(tx, id)
        .await?
        .ok_or_else(|| AppError::internal("evidence insert lost"))?;
    Ok(evidence_response(&row))
}

/// MCP wrapper: evidence submitted for an agent run must belong to the
/// calling principal before it enters the regular evidence use case.
pub async fn create_evidence_for_run_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    agent_run_id: Uuid,
    req: &CreateEvidenceRequest,
) -> Result<EvidenceResponse, AppError> {
    authz::require_scope(ctx, Scope::ResearchPropose)?;
    let Some(owner) = repo::agent_run_principal(tx, agent_run_id).await? else {
        return Err(AppError::not_found("agent run not found"));
    };
    if owner != ctx.principal_id {
        return Err(AppError::forbidden(
            "the research run does not belong to this principal",
        ));
    }
    create_evidence_inner_tx(tx, ctx, req).await
}

/// Non-idempotent variant.
pub async fn create_evidence(
    db: &Db,
    ctx: &Ctx,
    req: &CreateEvidenceRequest,
) -> Result<EvidenceResponse, AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    let res = create_evidence_tx(&mut tx, ctx, req).await?;
    tx.commit().await?;
    Ok(res)
}

/// `GET /research-evidence` — keyset over (created_at, id).
pub async fn list_evidence(
    db: &Db,
    ctx: &Ctx,
    f: &EvidenceFilters,
    after: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
) -> Result<(Vec<EvidenceResponse>, Option<(DateTime<Utc>, Uuid)>), AppError> {
    require_read(ctx)?;
    if let Some(s) = &f.review_status
        && !["pending", "verified", "rejected"].contains(&s.as_str())
    {
        return Err(AppError::validation(
            "unknown review_status filter",
            vec![Violation {
                field: "review_status".into(),
                code: "invalid".into(),
            }],
        ));
    }
    let mut tx = db.begin(ctx.workspace_id).await?;
    let rows = repo::list_evidence(
        &mut tx,
        f.company_id,
        f.contact_id,
        f.lead_proposal_id,
        f.review_status.as_deref(),
        after,
        limit + 1,
    )
    .await?;
    let has_more = rows.len() as i64 > limit;
    let rows: Vec<_> = rows.into_iter().take(limit as usize).collect();
    let next = if has_more {
        rows.last().map(|r| (r.created_at, r.id))
    } else {
        None
    };
    tx.commit().await?;
    Ok((rows.iter().map(evidence_response).collect(), next))
}

/// Filters for `GET /research-evidence` (B.3.12).
#[derive(Debug, Clone, Default)]
pub struct EvidenceFilters {
    pub company_id: Option<Uuid>,
    pub contact_id: Option<Uuid>,
    pub lead_proposal_id: Option<Uuid>,
    pub review_status: Option<String>,
}

pub async fn get_evidence(db: &Db, ctx: &Ctx, id: Uuid) -> Result<EvidenceResponse, AppError> {
    require_read(ctx)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let row = repo::get_evidence(&mut tx, id)
        .await?
        .ok_or_else(|| AppError::not_found("evidence not found"))?;
    let res = evidence_response(&row);
    tx.commit().await?;
    Ok(res)
}

/// `POST /research-evidence/{id}/reviews` — human + `research.write`
/// (§13.4; agents may never verify evidence). `research_evidence` carries
/// no version column, so the review is guarded by the pending-only
/// transition instead of If-Match.
pub async fn review_evidence(
    db: &Db,
    ctx: &Ctx,
    id: Uuid,
    req: &EvidenceReviewRequest,
) -> Result<EvidenceResponse, AppError> {
    authz::require_operator(ctx)?;
    authz::require_scope(ctx, Scope::ResearchWrite)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let row = repo::get_evidence(&mut tx, id)
        .await?
        .ok_or_else(|| AppError::not_found("evidence not found"))?;
    let new_status = match req.decision {
        EvidenceReviewDecision::Verified => "verified",
        EvidenceReviewDecision::Rejected => "rejected",
    };
    let transitioned = repo::review_evidence(&mut tx, id, new_status, ctx.principal_id).await?;
    if !transitioned {
        return Err(AppError::conflict(format!(
            "evidence already {}",
            row.review_status
        )));
    }
    outbox::emit(
        &mut tx,
        "research.evidence_reviewed",
        "research_evidence",
        id,
        2,
        serde_json::json!({"research_evidence_id": id}),
    )
    .await?;
    let mut entry = AuditEntry::new(
        ctx.principal_id,
        "evidence.reviewed",
        "research_evidence",
        ctx.request_id.clone(),
    )
    .with_target(id);
    entry.metadata = audit::details(&serde_json::json!({
        "decision": new_status,
        "reason": req.reason,
    }));
    audit::record(&mut tx, entry).await?;
    let row = repo::get_evidence(&mut tx, id)
        .await?
        .ok_or_else(|| AppError::internal("evidence lost"))?;
    tx.commit().await?;
    Ok(evidence_response(&row))
}
