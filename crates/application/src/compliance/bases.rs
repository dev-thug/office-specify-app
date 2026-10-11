//! `/communication-bases` use cases (B.2): list/get/create (+reviews,
//! +revoke). New bases are always `pending`; only a human review decides
//! `valid`/`rejected`; revoke withdraws an accepted basis (§11.4).

use chrono::{DateTime, Utc};
use contracts::compliance::{
    CommunicationBasisResponse, CreateCommunicationBasisRequest, ReviewBasisRequest,
};
use contracts::{AppError, Ctx};
use infrastructure::audit::{self, AuditEntry};
use infrastructure::db::Db;
use infrastructure::repo::compliance::{self as repo, BasisFilter, BasisRow};
use uuid::Uuid;

use super::{READ_ROLES, REVIEW_ROLES, WRITE_ROLES, field_violation};
use crate::authz;

pub fn basis_response(row: BasisRow) -> CommunicationBasisResponse {
    CommunicationBasisResponse {
        id: row.id,
        email_address_id: row.email_address_id,
        email_address: row.email_address,
        purpose: row.purpose,
        jurisdiction: row.jurisdiction,
        basis_type: row.basis_type,
        status: row.status,
        evidence_ref: row.evidence_ref,
        collected_at: row.collected_at,
        reviewed_by: row.reviewed_by,
        reviewed_at: row.reviewed_at,
        valid_until: row.valid_until,
        revoked_at: row.revoked_at,
        version: row.version,
        created_at: row.created_at,
    }
}

/// `GET /communication-bases` — cursor-paginated, allowlisted filters.
/// Returns up to `limit` rows plus the last key for the next cursor.
pub async fn list_bases(
    db: &Db,
    ctx: &Ctx,
    filter: BasisFilter,
    limit: u32,
) -> Result<
    (
        Vec<CommunicationBasisResponse>,
        Option<(DateTime<Utc>, Uuid)>,
    ),
    AppError,
> {
    authz::require_role(ctx, READ_ROLES)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let rows = repo::list_bases(&mut tx, &filter, i64::from(limit) + 1).await?;
    tx.rollback().await?;
    let mut rows = rows;
    let has_more = rows.len() as u32 > limit;
    rows.truncate(limit as usize);
    let last = if has_more {
        rows.last().map(|r| (r.created_at, r.id))
    } else {
        None
    };
    Ok((rows.into_iter().map(basis_response).collect(), last))
}

/// `GET /communication-bases/{id}`.
pub async fn get_basis(
    db: &Db,
    ctx: &Ctx,
    id: Uuid,
) -> Result<CommunicationBasisResponse, AppError> {
    authz::require_role(ctx, READ_ROLES)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let row = repo::get_basis(&mut tx, id).await?;
    tx.rollback().await?;
    row.map(basis_response)
        .ok_or_else(|| AppError::not_found("communication basis not found"))
}

/// `POST /communication-bases` — registers evidence of a contact basis.
/// Always created `pending` (B.2); clients may not supply status or the
/// reviewer fields (B.1.1 system-owned).
pub async fn create_basis(
    db: &Db,
    ctx: &Ctx,
    req: CreateCommunicationBasisRequest,
) -> Result<CommunicationBasisResponse, AppError> {
    authz::require_role(ctx, WRITE_ROLES)?;
    validate(&req)?;

    let mut tx = db.begin(ctx.workspace_id).await?;
    // The FK would also enforce this, but a 404 beats a 409 for a missing
    // address reference.
    if !infrastructure::repo::compliance_core::lock_email_address(&mut tx, req.email_address_id)
        .await?
    {
        tx.rollback().await?;
        return Err(AppError::not_found("email address not found"));
    }
    let id = repo::insert_basis(
        &mut tx,
        req.email_address_id,
        &req.purpose,
        req.jurisdiction.trim(),
        &req.basis_type,
        req.evidence_ref.trim(),
        req.collected_at,
        req.valid_until,
    )
    .await?;
    let row = repo::get_basis(&mut tx, id)
        .await?
        .ok_or_else(|| AppError::internal("basis missing after insert"))?;
    audit::record(
        &mut tx,
        AuditEntry::new(
            ctx.principal_id,
            "communication_basis.created",
            "communication_basis",
            ctx.request_id.clone(),
        )
        .with_target(id),
    )
    .await?;
    tx.commit().await?;
    Ok(basis_response(row))
}

fn validate(req: &CreateCommunicationBasisRequest) -> Result<(), AppError> {
    if domain::compliance::Purpose::parse(&req.purpose).is_none() {
        return Err(field_violation("purpose", "invalid", "unsupported purpose"));
    }
    if domain::compliance::BasisType::parse(&req.basis_type).is_none() {
        return Err(field_violation(
            "basis_type",
            "invalid",
            "unsupported basis_type",
        ));
    }
    let jurisdiction = req.jurisdiction.trim();
    if jurisdiction.is_empty() || jurisdiction.len() > 64 {
        return Err(field_violation(
            "jurisdiction",
            "invalid",
            "jurisdiction must be 1..=64 chars",
        ));
    }
    if req.evidence_ref.trim().is_empty() {
        return Err(field_violation(
            "evidence_ref",
            "required",
            "evidence_ref is required",
        ));
    }
    if let Some(until) = req.valid_until
        && until <= req.collected_at
    {
        return Err(field_violation(
            "valid_until",
            "invalid",
            "valid_until must be after collected_at",
        ));
    }
    if let Some(until) = req.valid_until
        && until <= Utc::now()
    {
        return Err(field_violation(
            "valid_until",
            "expired",
            "valid_until must be in the future",
        ));
    }
    Ok(())
}

/// `POST /communication-bases/{id}/reviews` — human decision only.
/// `If-Match` pins the reviewed version (B.1.4); only `pending` rows may
/// be decided — a decided basis is final (use revoke for `valid`).
pub async fn review_basis(
    db: &Db,
    ctx: &Ctx,
    id: Uuid,
    expected_version: i64,
    req: ReviewBasisRequest,
) -> Result<CommunicationBasisResponse, AppError> {
    authz::require_operator(ctx)?;
    authz::require_role(ctx, REVIEW_ROLES)?;
    let Some(decision) = domain::compliance::ReviewDecision::parse(&req.decision) else {
        return Err(field_violation(
            "decision",
            "invalid",
            "decision must be 'valid' or 'rejected'",
        ));
    };
    if decision == domain::compliance::ReviewDecision::Rejected && req.valid_until.is_some() {
        return Err(field_violation(
            "valid_until",
            "invalid",
            "valid_until is only meaningful for a 'valid' decision",
        ));
    }
    if req.valid_until.is_some_and(|until| until <= Utc::now()) {
        return Err(field_violation(
            "valid_until",
            "expired",
            "valid_until must be in the future",
        ));
    }

    let mut tx = db.begin(ctx.workspace_id).await?;
    match repo::apply_basis_review(
        &mut tx,
        id,
        expected_version,
        decision.as_str(),
        ctx.principal_id,
        req.valid_until,
    )
    .await?
    {
        Some(row) => {
            audit::record(
                &mut tx,
                AuditEntry::new(
                    ctx.principal_id,
                    "communication_basis.reviewed",
                    "communication_basis",
                    ctx.request_id.clone(),
                )
                .with_target(id),
            )
            .await?;
            tx.commit().await?;
            Ok(basis_response(row))
        }
        None => precondition_failure(&mut tx, id, expected_version, "basis already decided").await,
    }
}

/// `POST /communication-bases/{id}/revoke` — withdraws the basis; sends
/// re-check `valid_communication_basis` at dispatch, so revocation takes
/// effect without touching in-flight rows (§11.4).
pub async fn revoke_basis(
    db: &Db,
    ctx: &Ctx,
    id: Uuid,
    expected_version: i64,
) -> Result<CommunicationBasisResponse, AppError> {
    authz::require_operator(ctx)?;
    authz::require_role(ctx, REVIEW_ROLES)?;

    let mut tx = db.begin(ctx.workspace_id).await?;
    match repo::revoke_basis(&mut tx, id, expected_version).await? {
        Some(row) => {
            audit::record(
                &mut tx,
                AuditEntry::new(
                    ctx.principal_id,
                    "communication_basis.revoked",
                    "communication_basis",
                    ctx.request_id.clone(),
                )
                .with_target(id),
            )
            .await?;
            tx.commit().await?;
            Ok(basis_response(row))
        }
        None => precondition_failure(&mut tx, id, expected_version, "basis already revoked").await,
    }
}

/// A.6.1: zero updated rows → decide between 404, 412 and 409. The tx
/// drops uncommitted on the caller's error path (implicit rollback).
async fn precondition_failure<T>(
    tx: &mut infrastructure::db::Tx<'_>,
    id: Uuid,
    expected_version: i64,
    conflict_detail: &str,
) -> Result<T, AppError> {
    let current = repo::get_basis(tx, id).await?;
    match current {
        None => Err(AppError::not_found("communication basis not found")),
        Some(row) if row.version != expected_version => Err(AppError::new(
            contracts::ErrorCode::VersionMismatch,
            "the resource changed since it was read",
        )),
        Some(_) => Err(AppError::conflict(conflict_detail)),
    }
}
