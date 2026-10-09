//! Public unsubscribe capability endpoints (§11.2, B.4.4).
//!
//! The URL token is the only authority — no login, no e-mail re-entry, no
//! survey. GET never mutates state (mail security scanners prefetch
//! links); POST performs the workspace-wide marketing suppression and is
//! idempotent while an active marketing/all suppression exists. If a human
//! later releases the suppression, the same valid link can suppress again.

use contracts::compliance::{UnsubscribeConfirmationResponse, UnsubscribeResultResponse};
use contracts::{AppError, ErrorCode};
use infrastructure::audit::{self, AuditEntry};
use infrastructure::db::Db;
use infrastructure::public_tokens;
use infrastructure::repo::compliance_core;

/// B.4.4 success payload — fixed strings, verbatim from the spec example.
fn unsubscribed() -> UnsubscribeResultResponse {
    UnsubscribeResultResponse {
        status: "unsubscribed".into(),
        scope: "workspace_marketing".into(),
        message: "해당 발신 조직의 마케팅 이메일 수신 거부가 처리되었습니다.".into(),
    }
}

fn unknown_token() -> AppError {
    // Generalized 404 — must not reveal whether the address exists (B.4.4).
    AppError::not_found("unsubscribe link not found or expired")
}

/// `GET /public/v1/unsubscribe/{token}` — confirmation data only.
/// State is derived from the current active marketing/all suppression, not
/// the one-time token's consumed marker. A later approved release re-enables
/// confirmation for an old, still-valid link.
pub async fn confirmation(
    db: &Db,
    token: &str,
) -> Result<UnsubscribeConfirmationResponse, AppError> {
    let location = public_tokens::unsubscribe_token(db, token).await?;
    let mut tx = db.begin(location.workspace_id).await?;
    let Some(resolved) = compliance_core::resolve_unsubscribe_token(&mut tx, token).await? else {
        tx.rollback().await?;
        return Err(unknown_token());
    };
    if resolved.revoked_at.is_some() {
        tx.rollback().await?;
        return Err(unknown_token());
    }
    if !compliance_core::lock_email_address(&mut tx, resolved.email_address_id).await? {
        tx.rollback().await?;
        return Err(AppError::internal("token references a missing address"));
    }
    let active =
        compliance_core::active_suppressions(&mut tx, resolved.email_address_id, "marketing")
            .await?;
    tx.rollback().await?;
    Ok(UnsubscribeConfirmationResponse {
        status: if active.is_empty() {
            "pending_confirmation".into()
        } else {
            "unsubscribed".into()
        },
        scope: "workspace_marketing".into(),
        message: "해당 발신 조직의 마케팅 이메일 수신 거부를 확인합니다.".into(),
    })
}

/// `POST /public/v1/unsubscribe/{token}` — performs the suppression.
/// Repeat requests while any active marketing/all suppression exists return
/// the same success without new events or jobs. If that block was released,
/// an old valid token creates a fresh workspace-marketing suppression.
pub async fn unsubscribe(
    db: &Db,
    token: &str,
    request_id: &str,
) -> Result<UnsubscribeResultResponse, AppError> {
    let location = public_tokens::unsubscribe_token(db, token).await?;
    let mut tx = db.begin(location.workspace_id).await?;
    let Some(resolved) = compliance_core::resolve_unsubscribe_token(&mut tx, token).await? else {
        tx.rollback().await?;
        return Err(unknown_token());
    };
    if resolved.revoked_at.is_some() {
        tx.rollback().await?;
        return Err(unknown_token());
    }
    // A.6.4: the address row locks before the suppression decision.
    if !compliance_core::lock_email_address(&mut tx, resolved.email_address_id).await? {
        // FK integrity makes this unreachable; never pretend it succeeded.
        let _ = tx.rollback().await;
        return Err(AppError::internal("token references a missing address"));
    }
    // Consume under the address lock. A used token remains a valid capability
    // until revoked; it may need to recreate suppression after an approved
    // release. Repeated active requests do not reaffirm the core row, so they
    // create no duplicate suppression event/outbox/job.
    let consumed = compliance_core::consume_unsubscribe_token(&mut tx, token).await?;
    let current = if consumed {
        resolved.clone()
    } else {
        compliance_core::resolve_unsubscribe_token(&mut tx, token)
            .await?
            .filter(|row| row.revoked_at.is_none())
            .ok_or_else(unknown_token)?
    };
    if !consumed && current.used_at.is_none() {
        tx.rollback().await?;
        return Err(AppError::conflict(
            "unsubscribe token changed during processing",
        ));
    }
    let active =
        compliance_core::active_suppressions(&mut tx, current.email_address_id, "marketing")
            .await?;
    if let Some(existing) = active.first() {
        if consumed {
            audit::record(
                &mut tx,
                AuditEntry {
                    actor_principal_id: None,
                    requested_by: None,
                    approved_by: None,
                    job_id: None,
                    action: "suppression.unsubscribe_already_active".into(),
                    target_type: "suppression".into(),
                    target_id: Some(existing.id),
                    outcome: "ok".into(),
                    metadata: serde_json::json!({"token_id": location.token_id, "source": "unsubscribe_link"}),
                    request_id: request_id.to_string(),
                    trace_id: None,
                },
            ).await?;
            tx.commit().await?;
        } else {
            tx.rollback().await?;
        }
        return Ok(unsubscribed());
    }
    let suppression_id = compliance_core::insert_suppression(
        &mut tx,
        current.email_address_id,
        "marketing",
        "unsubscribe",
        "unsubscribe_link",
        None,
        Some(current.outbound_message_id),
    )
    .await?;
    audit::record(
        &mut tx,
        AuditEntry {
            // Pre-auth public action — no acting principal (§5.1).
            actor_principal_id: None,
            requested_by: None,
            approved_by: None,
            job_id: None,
            action: "suppression.unsubscribe".into(),
            target_type: "suppression".into(),
            target_id: Some(suppression_id),
            outcome: "ok".into(),
            metadata: serde_json::json!({
                "source": "unsubscribe_link",
                "token_id": location.token_id,
            }),
            request_id: request_id.to_string(),
            trace_id: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(unsubscribed())
}

/// `POST /public/v1/unsubscribe/{token}/one-click` — RFC 8058.
///
/// The one-click capability gate is OFF (release gate E01 — no delivery
/// provider is verified for `List-Unsubscribe-Post`, §11.3). The endpoint
/// exists and validates the token, but returns the documented disabled
/// response instead of processing the suppression: claiming success here
/// would silently drop a protocol-mandated unsubscribe.
pub async fn one_click(db: &Db, token: &str) -> Result<UnsubscribeResultResponse, AppError> {
    let _location = public_tokens::unsubscribe_token(db, token).await?;
    Err(AppError::new(
        ErrorCode::ProviderPolicyUnverified,
        "one-click unsubscribe is not enabled for this channel",
    ))
}
