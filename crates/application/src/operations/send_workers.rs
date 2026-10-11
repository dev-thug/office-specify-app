//! Restore lock management for outbound submission workers (T30).

use contracts::operations::ReleaseSendWorkersLockRequest;
use contracts::{AppError, Ctx, Role};
use infrastructure::db::{Db, Tx};
use infrastructure::repo::operations as repo;

/// Release the restore lock only after a human operator has reconciled
/// post-backup outbound submissions. The free-text reason is hashed before
/// it reaches append-only audit metadata.
pub async fn release_lock(
    db: &Db,
    ctx: &Ctx,
    req: &ReleaseSendWorkersLockRequest,
) -> Result<bool, AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    let was_locked = release_lock_tx(&mut tx, ctx, req).await?;
    tx.commit().await?;
    Ok(was_locked)
}

/// Transaction-owning variant for the REST idempotency wrapper.
pub async fn release_lock_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    req: &ReleaseSendWorkersLockRequest,
) -> Result<bool, AppError> {
    if !(ctx.role == Role::Admin || crate::authz::agent_admin(ctx)) {
        return Err(AppError::forbidden(
            "releasing the send-worker restore lock requires an Admin",
        ));
    }
    let reason = req.reason.trim();
    if reason.is_empty() || reason.chars().count() > 1000 {
        return Err(AppError::validation(
            "reason must contain 1 to 1000 characters",
            vec![contracts::Violation {
                field: "reason".into(),
                code: "out_of_range".into(),
            }],
        ));
    }
    let was_locked = repo::release_send_workers_lock(tx).await?;
    if !was_locked {
        return Err(AppError::conflict("send workers are not restore-locked"));
    }
    let reason_hash = infrastructure::crypto::sha256_hex(reason.as_bytes());
    infrastructure::audit::record(
        tx,
        infrastructure::audit::AuditEntry {
            actor_principal_id: Some(ctx.principal_id),
            requested_by: Some(ctx.principal_id),
            approved_by: None,
            job_id: None,
            action: "operations.send_workers_lock_released".into(),
            target_type: "workspace".into(),
            target_id: Some(ctx.workspace_id),
            outcome: "ok".into(),
            metadata: serde_json::json!({
                "send_workers_locked": false,
                "reason_hash": reason_hash,
                "reason_length": reason.chars().count(),
            }),
            request_id: ctx.request_id.clone(),
            trace_id: None,
        },
    )
    .await?;
    infrastructure::outbox::emit(
        tx,
        "operations.send_workers_lock_released",
        "workspace",
        ctx.workspace_id,
        1,
        serde_json::json!({ "send_workers_locked": false }),
    )
    .await?;
    Ok(false)
}
