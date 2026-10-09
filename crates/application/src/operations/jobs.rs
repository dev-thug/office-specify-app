//! `GET /jobs/{id}` and `POST /jobs/{id}/retry-requests` (B.2 운영 rows).
//!
//! Retry policy (§16.4, ADR-006): only `dead`/`retry_wait` re-queue;
//! `unknown` is NEVER retried — it needs reconcile, so the operator gets
//! B.5 `PROVIDER_SUBMISSION_OUTCOME_UNKNOWN`. External-effect kinds
//! additionally require a reason that lands in the audit entry.

use contracts::operations::{JobResponse, JobResultRef, JobRetryRequest, JobSummaryResponse};
use contracts::{AppError, Ctx, ErrorCode, Scope};
use infrastructure::db::{Db, Tx};
use infrastructure::jobs::{self, Job};
use infrastructure::repo::operations as repo;
use uuid::Uuid;

use crate::authz;

#[derive(Debug)]
pub struct JobListResult {
    pub items: Vec<JobSummaryResponse>,
    pub has_more: bool,
    pub cursor_seed: Option<(chrono::DateTime<chrono::Utc>, Uuid)>,
}

/// Row-level job visibility (B.2 "본인·리소스 접근"): `operations.read`
/// sees everything; otherwise only jobs the principal requested.
fn can_view(ctx: &Ctx, job: &Job) -> bool {
    matches!(ctx.role, contracts::Role::Admin | contracts::Role::Manager)
        || job.requested_by == Some(ctx.principal_id)
}

fn to_response(job: &Job) -> Result<JobResponse, AppError> {
    let state = contracts::operations::JobState::parse(&job.state)
        .ok_or_else(|| AppError::internal("job row has an invalid state"))?;
    let safe_payload = domain::operations::redact_job_payload(&job.payload);
    Ok(JobResponse {
        id: job.id,
        kind: job.kind.clone(),
        state,
        payload_version: job.payload_version,
        result_refs: result_refs(&safe_payload),
        requested_by: job.requested_by,
        attempt_count: job.attempt_count,
        max_attempts: jobs::MAX_ATTEMPTS,
        external_effect: job.external_effect,
        retryable: domain::operations::is_retryable_state(&job.state),
        retry_requires_reason: job.external_effect,
        available_at: job.available_at,
        lease_owner: job.lease_owner.clone(),
        lease_expires_at: job.lease_expires_at,
        last_error_code: job.last_error_code.clone(),
        finished_at: job.finished_at,
        created_at: job.created_at,
    })
}

fn result_refs(value: &serde_json::Value) -> Vec<JobResultRef> {
    fn append(value: &serde_json::Value, out: &mut Vec<JobResultRef>) {
        let Some(object) = value.as_object() else {
            return;
        };
        for (key, child) in object {
            if let Some(raw) = child.as_str()
                && let Ok(id) = Uuid::parse_str(raw)
            {
                out.push(JobResultRef {
                    reference_type: key.clone(),
                    id,
                });
            } else {
                append(child, out);
            }
        }
    }
    let mut refs = Vec::new();
    append(value, &mut refs);
    refs
}

fn summary_response(job: &Job) -> Result<JobSummaryResponse, AppError> {
    let state = contracts::operations::JobState::parse(&job.state)
        .ok_or_else(|| AppError::internal("job row has an invalid state"))?;
    Ok(JobSummaryResponse {
        id: job.id,
        kind: job.kind.clone(),
        state,
        payload_version: job.payload_version,
        attempt_count: job.attempt_count,
        last_error_code: job.last_error_code.clone(),
        available_at: job.available_at,
        lease_owner: job.lease_owner.clone(),
        lease_expires_at: job.lease_expires_at,
        requested_by: job.requested_by,
        finished_at: job.finished_at,
        created_at: job.created_at,
        retryable: domain::operations::is_retryable_state(&job.state),
        external_effect: job.external_effect,
    })
}

/// Fetch a job the caller may see. Existence is hidden — a job the caller
/// cannot access returns 404 (§12.3 "존재 은닉").
pub async fn get_job(db: &Db, ctx: &Ctx, job_id: Uuid) -> Result<JobResponse, AppError> {
    authz::require_scope(ctx, Scope::OperationsRead)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let job = repo::get_job(&mut tx, job_id).await?;
    tx.commit().await?;
    let Some(job) = job.filter(|j| can_view(ctx, j)) else {
        return Err(AppError::not_found("job not found"));
    };
    to_response(&job)
}

/// Workspace/owner-scoped job collection. State is a DDL CHECK enum and
/// kind is a short stable queue name; neither filter accepts arbitrary SQL
/// or wildcard syntax.
pub async fn list_jobs(
    db: &Db,
    ctx: &Ctx,
    state: Option<&str>,
    kind: Option<&str>,
    after: Option<(chrono::DateTime<chrono::Utc>, Uuid)>,
    limit: i64,
) -> Result<JobListResult, AppError> {
    authz::require_scope(ctx, Scope::OperationsRead)?;
    let state = match state {
        Some(value) => Some(
            contracts::operations::JobState::parse(value)
                .map(contracts::operations::JobState::as_str)
                .ok_or_else(|| {
                    AppError::validation(
                        "invalid job state filter",
                        vec![contracts::Violation {
                            field: "state".into(),
                            code: "invalid".into(),
                        }],
                    )
                })?,
        ),
        None => None,
    };
    let kind = match kind {
        Some(value) => {
            let value = value.trim();
            if value.is_empty()
                || value.len() > 96
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
            {
                return Err(AppError::validation(
                    "invalid job kind filter",
                    vec![contracts::Violation {
                        field: "kind".into(),
                        code: "invalid".into(),
                    }],
                ));
            }
            Some(value.to_string())
        }
        None => None,
    };
    let requested_by = (!matches!(ctx.role, contracts::Role::Admin | contracts::Role::Manager))
        .then_some(ctx.principal_id);
    let filter = repo::JobListFilter {
        state: state.map(str::to_string),
        kind,
        requested_by,
    };
    let mut tx = db.begin(ctx.workspace_id).await?;
    let rows = repo::list_jobs(&mut tx, &filter, after, limit + 1).await?;
    tx.commit().await?;
    let rows = rows
        .into_iter()
        .filter(|job| can_view(ctx, job))
        .collect::<Vec<_>>();
    let has_more = rows.len() as i64 > limit;
    let rows = rows.into_iter().take(limit as usize).collect::<Vec<_>>();
    let cursor_seed = has_more
        .then(|| rows.last().map(|job| (job.created_at, job.id)))
        .flatten();
    let items = rows
        .iter()
        .map(summary_response)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(JobListResult {
        items,
        has_more,
        cursor_seed,
    })
}

/// `POST /jobs/{id}/retry-requests`. Operations staff only (Admin/Manager).
/// The re-queue is state-guarded inside the UPDATE, so a duplicate request
/// after success lands on `queued` → CONFLICT (self-idempotent mutation;
/// the job row keeps its dedupe key so downstream effects stay deduped).
pub async fn request_retry(
    db: &Db,
    ctx: &Ctx,
    job_id: Uuid,
    req: &JobRetryRequest,
) -> Result<JobResponse, AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    let response = request_retry_tx(&mut tx, ctx, job_id, req).await?;
    tx.commit().await?;
    Ok(response)
}

/// Transaction-owning variant for the shared idempotency wrapper.
pub async fn request_retry_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    job_id: Uuid,
    req: &JobRetryRequest,
) -> Result<JobResponse, AppError> {
    // B.2 "운영" — ops staff. Agents never reach this (role Agent fails).
    authz::require_role(ctx, &[contracts::Role::Admin, contracts::Role::Manager])?;
    authz::require_scope(ctx, Scope::OperationsRead)?;

    let job = repo::get_job(tx, job_id)
        .await?
        .filter(|j| can_view(ctx, j))
        .ok_or_else(|| AppError::not_found("job not found"))?;

    if domain::operations::is_unknown_state(&job.state) {
        // ADR-006: unknown outcomes are reconciled, never retried.
        return Err(AppError::new(
            ErrorCode::ProviderSubmissionOutcomeUnknown,
            "job outcome is unknown; reconcile before any retry",
        ));
    }
    if !domain::operations::is_retryable_state(&job.state) {
        return Err(AppError::conflict(format!(
            "job in state '{}' is not retryable",
            job.state
        )));
    }

    let external = job.external_effect;
    let reason = req.reason.as_deref().map(str::trim).unwrap_or_default();
    if reason.chars().count() > 1000 {
        return Err(AppError::validation(
            "reason must be at most 1000 characters",
            vec![contracts::Violation {
                field: "reason".into(),
                code: "too_long".into(),
            }],
        ));
    }
    if external && reason.is_empty() {
        // §16.4: external-effect retries need a recorded operator reason.
        return Err(AppError::validation(
            "reason is required to retry an external-effect job",
            vec![contracts::Violation {
                field: "reason".into(),
                code: "required".into(),
            }],
        )
        .with_status(422));
    }

    let updated = repo::requeue_job_for_retry(tx, job_id)
        .await?
        .ok_or_else(|| {
            // Lost a state race between the read and the guarded update.
            AppError::conflict("job state changed; reload and retry the request")
        })?;

    let reason_hash = if reason.is_empty() {
        None
    } else {
        Some(infrastructure::crypto::sha256_hex(reason.as_bytes()))
    };
    let mut audit_metadata = serde_json::json!({
        "kind": job.kind,
        "previous_state": job.state,
        "previous_attempt_count": job.attempt_count,
        "external_effect": external,
    });
    if let Some(reason_hash) = &reason_hash {
        audit_metadata["reason_hash"] = serde_json::Value::String(reason_hash.clone());
        audit_metadata["reason_length"] = serde_json::Value::from(reason.chars().count() as u64);
    }
    infrastructure::audit::record(
        tx,
        infrastructure::audit::AuditEntry {
            actor_principal_id: Some(ctx.principal_id),
            requested_by: job.requested_by,
            approved_by: None,
            job_id: Some(job.id),
            action: "job.retry_requested".into(),
            target_type: "job".into(),
            target_id: Some(job.id),
            outcome: "ok".into(),
            metadata: audit_metadata,
            request_id: ctx.request_id.clone(),
            trace_id: None,
        },
    )
    .await?;
    infrastructure::outbox::emit(
        tx,
        "job.retry_requested",
        "job",
        job.id,
        1,
        serde_json::json!({
            "job_id": job.id,
            "previous_attempt_count": job.attempt_count,
            "attempt_count": updated.attempt_count,
        }),
    )
    .await?;
    to_response(&updated)
}
