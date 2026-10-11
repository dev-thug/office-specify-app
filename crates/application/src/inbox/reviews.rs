//! Human classification review (B.2: `POST /email-messages/{id}/
//! classification-reviews`, §9.4). Humans only; records the reviewer and —
//! when the reviewed class pauses follow-ups — enqueues
//! `campaigns.pause_address` (safety stop; a reply's text alone never
//! creates a suppression — §11 blocks via the unsubscribe endpoints /
//! compliance review, not inbox text).

use contracts::inbox::{Classification, ClassificationReviewRequest, ClassificationReviewResponse};
use contracts::{AppError, Ctx, ErrorCode, Scope};
use uuid::Uuid;

use crate::authz;
use infrastructure::db::Tx;
use infrastructure::repo::inbox as repo;
use infrastructure::{audit, audit::AuditEntry};

/// Review a message's classification. Idempotent by nature (the row just
/// records the latest human decision); audits every review.
pub async fn review_classification(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    message_id: Uuid,
    req: &ClassificationReviewRequest,
) -> Result<ClassificationReviewResponse, AppError> {
    authz::require_operator(ctx)?;
    authz::require_scope(ctx, Scope::MailRead)?;

    let Some(msg) = repo::get_message(tx, message_id).await? else {
        return Err(AppError::not_found("message not found"));
    };
    match authz::mailbox_grant(tx, ctx, msg.mail_account_id).await? {
        Some(g) if g.can_read => {}
        Some(_) => {
            return Err(AppError::new(
                ErrorCode::MailboxAccessDenied,
                "no access to this mailbox",
            ));
        }
        None => {
            return Err(
                AppError::new(ErrorCode::MailboxAccessDenied, "no access to this mailbox")
                    .with_status(404),
            );
        }
    }

    let updated = repo::set_classification(
        tx,
        message_id,
        req.classification.as_str(),
        ctx.principal_id,
    )
    .await?
    .ok_or_else(|| AppError::not_found("message not found"))?;

    // Safety stop per §9.4: pause follow-up sends to the reviewed address.
    if let Some(reason) = req.classification.pause_reason()
        && let Some(from) = repo::from_address_id(tx, message_id).await?
    {
        repo::enqueue_coalesced(
            tx,
            "campaigns.pause_address",
            &serde_json::json!({
                "email_address_id": from,
                "reason": reason,
            }),
            &format!("pause_address:{from}:{reason}"),
        )
        .await?;
    }

    audit::record(
        tx,
        AuditEntry {
            actor_principal_id: Some(ctx.principal_id),
            requested_by: None,
            approved_by: None,
            job_id: None,
            action: "message.classification_reviewed".into(),
            target_type: "email_message".into(),
            target_id: Some(message_id),
            outcome: "ok".into(),
            metadata: serde_json::json!({
                "classification": req.classification.as_str(),
                // Reviewer's free-text reason — audit metadata only (no
                // column, per integration-contracts).
                "reason": req.reason,
            }),
            request_id: ctx.request_id.clone(),
            trace_id: None,
        },
    )
    .await?;

    Ok(ClassificationReviewResponse {
        id: updated.id,
        classification: Classification::parse(&updated.classification)
            .unwrap_or(Classification::Unknown),
        classification_reviewed_by: ctx.principal_id,
    })
}
