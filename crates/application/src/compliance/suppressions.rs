//! `/suppressions` use cases (B.2): list/get/create + release-requests.
//! All writes go through `compliance_core::insert_suppression` (the shared
//! core owns the A.6.4 lock order, `suppression_events`, the outbox event
//! and the enrollments-pause job) — this module adds authz, validation,
//! audit and the approval request.

use chrono::{DateTime, Duration, Utc};
use contracts::compliance::{
    CreateSuppressionRequest, ReleaseRequestResponse, ReleaseSuppressionRequest,
    SuppressionResponse,
};
use contracts::{AppError, AuthMethod, Ctx, ErrorCode, PrincipalKind, Role, Scope};
use infrastructure::audit::{self, AuditEntry};
use infrastructure::db::Db;
use infrastructure::ratelimit::{FailMode, RateLimiter};
use infrastructure::repo::approvals::{self as approvals_repo, ApprovalTarget};
use infrastructure::repo::compliance::{self as repo, SuppressionFilter, SuppressionRow};
use infrastructure::repo::compliance_core;
use uuid::Uuid;

use super::{READ_ROLES, REVIEW_ROLES, WRITE_ROLES, field_violation};
use crate::authz;

/// Suppression-release approvals expire after 7 days — a decision taken on
/// a stale request must not release a block that moved on since (A.6.3).
const RELEASE_REQUEST_TTL: Duration = Duration::days(7);

/// §5.3 "제한적 추가" cap: agents may create at most this many suppressions
/// per credential per hour (rolling ≈ the facility's fixed window).
const AGENT_SUPPRESSION_LIMIT: u32 = 20;
const AGENT_SUPPRESSION_WINDOW: std::time::Duration = std::time::Duration::from_secs(3600);

pub fn suppression_response(row: SuppressionRow) -> SuppressionResponse {
    SuppressionResponse {
        id: row.id,
        email_address_id: row.email_address_id,
        email_address: row.email_address,
        purpose_scope: row.purpose_scope,
        reason: row.reason,
        status: if row.released_at.is_some() {
            "released".into()
        } else {
            "active".into()
        },
        source_outbound_id: row.source_outbound_id,
        created_by: row.created_by,
        released_at: row.released_at,
        released_by: row.released_by,
        release_evidence_ref: row.release_evidence_ref,
        version: row.version,
        created_at: row.created_at,
    }
}

/// `GET /suppressions` — "제한된 검색": allowlisted fields + cursor.
pub async fn list_suppressions(
    db: &Db,
    ctx: &Ctx,
    filter: SuppressionFilter,
    limit: u32,
) -> Result<(Vec<SuppressionResponse>, Option<(DateTime<Utc>, Uuid)>), AppError> {
    authz::require_role(ctx, READ_ROLES)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let rows = repo::list_suppressions(&mut tx, &filter, i64::from(limit) + 1).await?;
    tx.rollback().await?;
    let mut rows = rows;
    let has_more = rows.len() as u32 > limit;
    rows.truncate(limit as usize);
    let last = if has_more {
        rows.last().map(|r| (r.created_at, r.id))
    } else {
        None
    };
    Ok((rows.into_iter().map(suppression_response).collect(), last))
}

/// `GET /suppressions/{id}`.
pub async fn get_suppression(
    db: &Db,
    ctx: &Ctx,
    id: Uuid,
) -> Result<SuppressionResponse, AppError> {
    authz::require_role(ctx, READ_ROLES)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let row = repo::get_suppression(&mut tx, id).await?;
    tx.rollback().await?;
    row.map(suppression_response)
        .ok_or_else(|| AppError::not_found("suppression not found"))
}

/// Result of `create_suppression` — `created=false` when an identical
/// active suppression was reaffirmed instead of duplicated (§11.1 keeps
/// each distinct reason, never merges them).
pub struct SuppressionCreated {
    pub response: SuppressionResponse,
    pub created: bool,
}

/// §5.3 "limited add" quota — every syntactically valid `POST
/// /suppressions` by an agent principal counts against the per-credential
/// cap (reaffirms included: they still write an event row + pause job).
/// Keyed on the service credential; falls back to the principal id for
/// non-credential auth. Skipped when no limiter is configured (dev/test).
async fn agent_create_quota(ctx: &Ctx, rate_limiter: Option<&RateLimiter>) -> Result<(), AppError> {
    if ctx.principal_kind != PrincipalKind::Agent {
        return Ok(());
    }
    let Some(rl) = rate_limiter else {
        return Ok(());
    };
    let key = match ctx.auth {
        AuthMethod::ServiceCredential { credential_id } => {
            format!("suppressions:create:{credential_id}")
        }
        _ => format!("suppressions:create:principal:{}", ctx.principal_id),
    };
    // Fail closed (§16.3): when the limiter is unreachable the limited
    // capability pauses rather than running unbounded.
    rl.require(
        &key,
        AGENT_SUPPRESSION_LIMIT,
        AGENT_SUPPRESSION_WINDOW,
        FailMode::Closed,
    )
    .await?;
    Ok(())
}

/// §5.3 "limited add" scope — Member (업무 범위) and Agent (제한적 추가)
/// may only suppress addresses visible to them through the CRM/mailbox
/// ACL: linked to a contact (humans share CRM read; agents need the
/// `crm.read` scope) or appearing in a thread of a mailbox they hold
/// `can_read` on (agents need `mail.read`). The check runs before any
/// existence disclosure — out-of-scope and nonexistent addresses both
/// get 403 FORBIDDEN.
async fn require_address_in_scope(
    tx: &mut infrastructure::db::Tx<'_>,
    ctx: &Ctx,
    email_address_id: Uuid,
) -> Result<(), AppError> {
    if matches!(ctx.role, Role::Admin | Role::Manager) {
        return Ok(());
    }
    let via_contact = ctx.scopes.contains(Scope::CrmRead);
    let via_mailbox = ctx.scopes.contains(Scope::MailRead);
    let visible = (via_contact || via_mailbox)
        && repo::address_visible_to(
            tx,
            ctx.principal_id,
            email_address_id,
            via_contact,
            via_mailbox,
        )
        .await?;
    if visible {
        Ok(())
    } else {
        Err(AppError::forbidden(
            "email address is outside your visible scope",
        ))
    }
}

/// `POST /suppressions` — record a block via the shared core. Repeat calls
/// with the same (address, scope, reason) reaffirm the existing row — no
/// version precondition needed (B.1.4). Member/Agent are limited to
/// in-scope addresses (§5.3); agents are additionally rate-capped per
/// credential (429 RATE_LIMITED).
pub async fn create_suppression(
    db: &Db,
    ctx: &Ctx,
    rate_limiter: Option<&RateLimiter>,
    req: CreateSuppressionRequest,
) -> Result<SuppressionCreated, AppError> {
    authz::require_role(ctx, WRITE_ROLES)?;
    if domain::compliance::PurposeScope::parse(&req.purpose_scope).is_none() {
        return Err(field_violation(
            "purpose_scope",
            "invalid",
            "purpose_scope must be 'marketing' or 'all'",
        ));
    }
    if domain::compliance::SuppressionReason::parse(&req.reason).is_none() {
        return Err(field_violation("reason", "invalid", "unsupported reason"));
    }
    agent_create_quota(ctx, rate_limiter).await?;

    let mut tx = db.begin(ctx.workspace_id).await?;
    if let Err(e) = require_address_in_scope(&mut tx, ctx, req.email_address_id).await {
        tx.rollback().await?;
        return Err(e);
    }
    // A.6.4 single lock-order rule: the address row locks first, before any
    // suppression decision.
    if !compliance_core::lock_email_address(&mut tx, req.email_address_id).await? {
        tx.rollback().await?;
        return Err(AppError::not_found("email address not found"));
    }
    if let Some(outbound_id) = req.source_outbound_id
        && !repo::outbound_belongs_to_address(&mut tx, req.email_address_id, outbound_id).await?
    {
        tx.rollback().await?;
        return Err(field_violation(
            "source_outbound_id",
            "invalid",
            "outbound message does not target this address",
        ));
    }
    let suppression_id = compliance_core::insert_suppression(
        &mut tx,
        req.email_address_id,
        &req.purpose_scope,
        &req.reason,
        "manual",
        Some(ctx.principal_id),
        req.source_outbound_id,
    )
    .await?;
    let created = repo::last_suppression_action(&mut tx, suppression_id)
        .await?
        .as_deref()
        == Some("created");
    let row = repo::get_suppression(&mut tx, suppression_id)
        .await?
        .ok_or_else(|| AppError::internal("suppression missing after insert"))?;
    audit::record(
        &mut tx,
        AuditEntry::new(
            ctx.principal_id,
            if created {
                "suppression.created"
            } else {
                "suppression.reaffirmed"
            },
            "suppression",
            ctx.request_id.clone(),
        )
        .with_target(suppression_id),
    )
    .await?;
    tx.commit().await?;
    Ok(SuppressionCreated {
        response: suppression_response(row),
        created,
    })
}

/// `POST /suppressions/{id}/release-requests` — evidence + reason create a
/// pending approval request (`suppression_release`); never a direct
/// release (B.2). Agents may never request release (§5.4, §13.2) and the
/// requester's free text lands in the audit metadata (integration contract).
pub async fn request_release(
    db: &Db,
    ctx: &Ctx,
    suppression_id: Uuid,
    req: ReleaseSuppressionRequest,
) -> Result<ReleaseRequestResponse, AppError> {
    authz::require_role(ctx, REVIEW_ROLES)?;
    if req.evidence_ref.trim().is_empty() {
        return Err(field_violation(
            "evidence_ref",
            "required",
            "release evidence is required",
        ));
    }
    if req.reason.trim().is_empty() {
        return Err(field_violation(
            "reason",
            "required",
            "a release reason is required",
        ));
    }

    let mut tx = db.begin(ctx.workspace_id).await?;
    let Some(initial) = repo::get_suppression(&mut tx, suppression_id).await? else {
        tx.rollback().await?;
        return Err(AppError::not_found("suppression not found"));
    };
    // A.6.4 lock order: the email address comes before the suppression row.
    if !compliance_core::lock_email_address(&mut tx, initial.email_address_id).await? {
        tx.rollback().await?;
        return Err(AppError::not_found("suppression not found"));
    }
    let Some(supp) = repo::get_suppression_for_update(&mut tx, suppression_id).await? else {
        tx.rollback().await?;
        return Err(AppError::not_found("suppression not found"));
    };
    if supp.released_at.is_some() {
        tx.rollback().await?;
        return Err(AppError::conflict("suppression is already released"));
    }
    if repo::pending_release_request(&mut tx, suppression_id)
        .await?
        .is_some()
    {
        tx.rollback().await?;
        return Err(AppError::new(
            ErrorCode::RequestInProgress,
            "a release request is already pending for this suppression",
        ));
    }

    // Snapshot pins the exact suppression state being approved (A.6.3) —
    // a later release/reaffirm invalidates the request.
    let snapshot_hash = infrastructure::crypto::sha256_bytes(
        serde_json::json!({
            "suppression_id": supp.id,
            "version": supp.version,
            "email_address_id": supp.email_address_id,
            "purpose_scope": supp.purpose_scope,
            "reason": supp.reason,
        })
        .to_string()
        .as_bytes(),
    );
    let expires_at = Utc::now() + RELEASE_REQUEST_TTL;
    let request_id = approvals_repo::insert_request(
        &mut tx,
        ctx.principal_id,
        ApprovalTarget::SuppressionRelease(suppression_id),
        &snapshot_hash,
        expires_at,
    )
    .await?;
    audit::record(
        &mut tx,
        AuditEntry {
            actor_principal_id: Some(ctx.principal_id),
            requested_by: Some(ctx.principal_id),
            approved_by: None,
            job_id: None,
            action: "suppression.release_requested".into(),
            target_type: "suppression".into(),
            target_id: Some(suppression_id),
            outcome: "ok".into(),
            metadata: serde_json::json!({
                "approval_request_id": request_id,
                "reason": req.reason,
                "evidence_ref": req.evidence_ref,
            }),
            request_id: ctx.request_id.clone(),
            trace_id: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(ReleaseRequestResponse {
        approval_request_id: request_id,
        suppression_id,
        status: "pending".into(),
        expires_at,
    })
}
