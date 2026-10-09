//! `compliance.release_suppression` job body (integration contract:
//! enqueued by the approvals module on an approved `suppression_release`
//! decision). The release itself is `compliance_core::release_suppression`
//! — human-authorized only, re-verified inside this job's transaction.

use contracts::AppError;
use infrastructure::audit::{self, AuditEntry};
use infrastructure::db::Tx;
use infrastructure::outbox;
use infrastructure::repo::compliance::{self as repo};
use infrastructure::repo::compliance_core;
use uuid::Uuid;

/// Payload `{approval_request_id, suppression_id}` (payload_version 1 —
/// unknown fields ignored, missing fields fail the job).
pub async fn handle(
    tx: &mut Tx<'_>,
    job_id: Uuid,
    payload: &serde_json::Value,
) -> Result<(), AppError> {
    let approval_request_id = payload
        .get("approval_request_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| AppError::validation("missing approval_request_id", vec![]))?;
    let suppression_id = payload
        .get("suppression_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| AppError::validation("missing suppression_id", vec![]))?;

    // Re-verify inside the job tx: the request must be an approved
    // suppression_release pointing at this suppression with a recorded
    // human decision. Anything else is a stale/forged job → no-op, not an
    // error (the job is finished; there is just nothing authorized).
    let ctx = repo::release_context(tx, approval_request_id).await?;
    let authorized = ctx.as_ref().is_some_and(|r| {
        r.action_type == "suppression_release"
            && r.status == "approved"
            && r.suppression_id == Some(suppression_id)
            && r.decided_by.is_some()
    });
    if !authorized {
        return Ok(());
    }
    let ctx = ctx.expect("checked above");
    let decided_by = ctx.decided_by.expect("checked above");

    let Some(supp) = repo::get_suppression(tx, suppression_id).await? else {
        return Ok(());
    };
    // Idempotent: a released suppression needs nothing more.
    if supp.released_at.is_some() {
        return Ok(());
    }
    // A.6.4 lock order: address first, then the suppression decision.
    compliance_core::lock_email_address(tx, supp.email_address_id).await?;
    // The release evidence: the approver's recorded reason, else a stable
    // pointer at the approval request itself (DDL requires non-NULL).
    let evidence = ctx
        .decision_reason
        .filter(|r| !r.trim().is_empty())
        .unwrap_or_else(|| format!("approval_request:{approval_request_id}"));
    if !compliance_core::release_suppression(tx, suppression_id, decided_by, &evidence).await? {
        // Lost a race against another release path — already done.
        return Ok(());
    }

    outbox::emit(
        tx,
        outbox::event_types::SUPPRESSION_RELEASED,
        "suppression",
        suppression_id,
        supp.version + 1,
        serde_json::json!({
            "suppression_id": suppression_id,
            "email_address_id": supp.email_address_id,
            "purpose_scope": supp.purpose_scope,
            "released_by": decided_by,
        }),
    )
    .await?;
    // Worker-side actions audit under the workspace's system principal (A.7).
    let actor = repo::system_principal_id(tx).await?;
    audit::record(
        tx,
        AuditEntry {
            actor_principal_id: actor,
            requested_by: Some(ctx.requested_by),
            approved_by: Some(decided_by),
            job_id: Some(job_id),
            action: "suppression.released".into(),
            target_type: "suppression".into(),
            target_id: Some(suppression_id),
            outcome: "ok".into(),
            metadata: serde_json::json!({ "approval_request_id": approval_request_id }),
            request_id: String::new(),
            trace_id: None,
        },
    )
    .await?;
    Ok(())
}
