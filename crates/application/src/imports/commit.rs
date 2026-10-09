//! `POST /imports/{id}/commit` — snapshot-hash check → `committing` →
//! `imports.commit` job (B.3.13). Runs inside the caller's idempotency tx.

use chrono::Utc;
use contracts::imports::{CommitImportRequest, JobAcceptedResponse};
use contracts::{AppError, Ctx, ErrorCode, Scope, Violation};
use infrastructure::audit::{self, AuditEntry};
use infrastructure::db::Tx;
use infrastructure::jobs;
use infrastructure::repo::imports as repo;
use uuid::Uuid;

use crate::authz;

pub async fn request_commit(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    import_id: Uuid,
    req: &CommitImportRequest,
) -> Result<JobAcceptedResponse, AppError> {
    authz::require_scope(ctx, Scope::CrmImport)?;

    let job = repo::lock_import_job(tx, import_id)
        .await?
        .ok_or_else(|| AppError::not_found("import not found"))?;
    if job.kind() != "import" {
        return Err(AppError::conflict("not an import job"));
    }
    if job.status != "ready" {
        return Err(AppError::conflict(format!(
            "import is {status}, expected ready",
            status = job.status
        )));
    }

    // B.3.13: the client must prove it saw the exact validation result —
    // the hash pins (object content + mapping + per-row outcomes).
    let expected = job.validation_hash.as_deref().unwrap_or(&[]);
    let given = decode_hex64(&req.validation_hash).ok_or_else(|| {
        AppError::validation(
            "validation_hash must be 64 hex characters",
            vec![Violation {
                field: "validation_hash".into(),
                code: "invalid".into(),
            }],
        )
    })?;
    if given != expected {
        return Err(AppError::new(
            ErrorCode::SnapshotMismatch,
            "validation_hash does not match the validated snapshot",
        ));
    }

    let invalid_rows = job
        .summary
        .get("invalid_rows")
        .and_then(|x| x.as_i64())
        .unwrap_or(0);
    if req.invalid_row_policy == contracts::imports::InvalidRowPolicy::Reject && invalid_rows > 0 {
        return Err(
            AppError::new(ErrorCode::ValidationFailed, "invalid rows present")
                .with_status(422)
                .with_violations(vec![Violation {
                    field: "invalid_row_policy".into(),
                    code: "invalid_rows_present".into(),
                }]),
        );
    }

    // Stash the commit policy where the job can read it, then transition.
    repo::merge_summary(
        tx,
        import_id,
        &serde_json::json!({
            "invalid_row_policy": match req.invalid_row_policy {
                contracts::imports::InvalidRowPolicy::Skip => "skip",
                contracts::imports::InvalidRowPolicy::Reject => "reject",
            }
        }),
    )
    .await?;
    if !repo::transition_status(tx, import_id, &["ready"], "committing").await? {
        return Err(AppError::conflict("import changed concurrently"));
    }
    jobs::enqueue(
        tx,
        "imports.commit",
        1,
        serde_json::json!({"import_job_id": import_id}),
        Utc::now(),
        Some(&format!("imports.commit:{import_id}")),
    )
    .await?;
    audit::record(
        tx,
        AuditEntry::new(
            ctx.principal_id,
            "import.commit_requested",
            "import_job",
            ctx.request_id.clone(),
        )
        .with_target(import_id),
    )
    .await?;
    Ok(JobAcceptedResponse {
        import_id,
        status: contracts::imports::ImportStatus::Committing,
    })
}

fn decode_hex64(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..32)
        .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok())
        .collect()
}
