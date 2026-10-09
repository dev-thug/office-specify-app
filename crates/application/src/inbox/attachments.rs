//! Attachment download (B.2 `POST /message-attachments/{id}/
//! download-requests`, §15.2). The POST re-checks `can_read` on the owning
//! mailbox and mints a short-lived HMAC token bound to
//! `(attachment, principal, expiry)`; the GET verifies the token AND
//! re-checks the grant, then streams bytes from the object store.
//! `storage_key` is never exposed.

use std::time::Duration;

use chrono::{DateTime, Utc};
use contracts::inbox::DownloadRequestResponse;
use contracts::{AppError, Ctx, ErrorCode, Scope, Violation};
use uuid::Uuid;

use crate::authz;
use infrastructure::crypto;
use infrastructure::db::Tx;
use infrastructure::repo::inbox as repo;
use infrastructure::storage::{LocalObjectStore, ObjectStore};
use infrastructure::{audit, audit::AuditEntry};

/// Download token TTL — short-lived per §15.2.
pub const DOWNLOAD_TTL: Duration = Duration::from_secs(300);

pub struct AttachmentContent {
    pub filename: String,
    pub media_type: String,
    pub bytes: Vec<u8>,
}

/// Load the attachment and enforce mailbox `can_read`; quarantined objects
/// (`scan_status='rejected'`) are not downloadable.
async fn attachment_for_download(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    attachment_id: Uuid,
) -> Result<repo::AttachmentObjectRow, AppError> {
    authz::require_scope(ctx, Scope::MailRead)?;
    let Some(att) = repo::get_attachment_object(tx, attachment_id).await? else {
        return Err(AppError::not_found("attachment not found"));
    };
    match authz::mailbox_grant(tx, ctx, att.mail_account_id).await? {
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
    if att.scan_status == "rejected" {
        return Err(AppError::validation(
            "attachment is quarantined",
            vec![Violation {
                field: "id".into(),
                code: "attachment_quarantined".into(),
            }],
        )
        .with_status(422));
    }
    Ok(att)
}

fn sign_token(hmac_key: &[u8], attachment_id: Uuid, principal_id: Uuid, exp: u64) -> String {
    let body = format!("{attachment_id}.{principal_id}.{exp}");
    let sig = crypto::b64_encode(&crypto::hmac_sha256(hmac_key, body.as_bytes()));
    format!("{body}.{sig}")
}

fn verify_token(
    hmac_key: &[u8],
    attachment_id: Uuid,
    principal_id: Uuid,
    token: &str,
) -> Result<(), AppError> {
    let invalid = || AppError::new(ErrorCode::Forbidden, "invalid or expired download token");
    let mut parts = token.split('.');
    let (Some(att), Some(pid), Some(exp_s), Some(sig)) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(invalid());
    };
    if parts.next().is_some() {
        return Err(invalid());
    }
    if att != attachment_id.to_string() || pid != principal_id.to_string() {
        return Err(invalid());
    }
    let exp: u64 = exp_s.parse().map_err(|_| invalid())?;
    let body = format!("{att}.{pid}.{exp}");
    let sig_bytes = crypto::b64_decode(sig).ok_or_else(invalid)?;
    if !crypto::hmac_sha256_verify(hmac_key, body.as_bytes(), &sig_bytes) {
        return Err(invalid());
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if exp < now {
        return Err(invalid());
    }
    Ok(())
}

/// `POST /message-attachments/{id}/download-requests`.
pub async fn request_download(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    attachment_id: Uuid,
    hmac_key: &[u8],
) -> Result<DownloadRequestResponse, AppError> {
    let _att = attachment_for_download(tx, ctx, attachment_id).await?;
    let exp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .saturating_add(DOWNLOAD_TTL)
        .as_secs();
    let token = sign_token(hmac_key, attachment_id, ctx.principal_id, exp);
    audit::record(
        tx,
        AuditEntry::new(
            ctx.principal_id,
            "attachment.download_requested",
            "message_attachment",
            ctx.request_id.clone(),
        )
        .with_target(attachment_id),
    )
    .await?;
    Ok(DownloadRequestResponse {
        attachment_id,
        download_url: format!("/api/v1/message-attachments/{attachment_id}/download?token={token}"),
        expires_at: DateTime::from_timestamp(exp as i64, 0).unwrap_or_else(Utc::now),
    })
}

/// `GET /message-attachments/{id}/download?token=…` — verifies the
/// capability and re-checks the grant (§15.2), then reads the object.
pub async fn download(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    attachment_id: Uuid,
    token: &str,
    hmac_key: &[u8],
    store: &LocalObjectStore,
) -> Result<AttachmentContent, AppError> {
    verify_token(hmac_key, attachment_id, ctx.principal_id, token)?;
    let att = attachment_for_download(tx, ctx, attachment_id).await?;
    let bytes = store.get(&att.storage_key).await?;
    audit::record(
        tx,
        AuditEntry::new(
            ctx.principal_id,
            "attachment.downloaded",
            "message_attachment",
            ctx.request_id.clone(),
        )
        .with_target(attachment_id),
    )
    .await?;
    Ok(AttachmentContent {
        filename: att.filename,
        media_type: att.media_type,
        bytes,
    })
}
