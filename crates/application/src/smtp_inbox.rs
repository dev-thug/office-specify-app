//! Admin-controlled IMAP synchronization for the mailbox behind an existing
//! SMTP profile. The worker reads only INBOX and stores every accepted
//! provider message through the shared inbox ingest path.

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use contracts::{AppError, Ctx, Violation};
use infrastructure::{
    audit::{self, AuditEntry},
    config::Config,
    db::{Db, Tx},
    jobs,
    providers::{
        ProviderError, SecretValue,
        imap_reply::{self, ImapInboxSource},
    },
    repo::{mailboxes, smtp, smtp_replies as repo},
    secrets::{FileSecretStore, SecretStore},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

const POLL_KIND: &str = "smtp_inbox.poll";
const POLL_INTERVAL: ChronoDuration = ChronoDuration::seconds(60);
const CONTINUATION_DELAY: ChronoDuration = ChronoDuration::seconds(2);
const FAILURE_RETRY: ChronoDuration = ChronoDuration::minutes(15);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxSyncStatus {
    pub smtp_connection_id: Uuid,
    pub mail_account_id: Uuid,
    pub enabled: bool,
    pub imap_host: String,
    pub imap_port: u16,
    pub folder: String,
    pub cursor_uidvalidity: Option<u32>,
    pub cursor_uid: u32,
    pub imported_messages: u64,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_error_code: Option<String>,
    pub next_sync_at: Option<DateTime<Utc>>,
    pub active_job_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxSyncQueued {
    pub job_id: Uuid,
    pub state: String,
}

fn secret_store(config: &Config) -> Result<FileSecretStore, AppError> {
    Ok(FileSecretStore::new(
        &config.secrets_file,
        config.secret_store_key.ok_or_else(|| {
            AppError::dependency_unavailable("SMTP credential encryption is not configured")
        })?,
    ))
}

fn validation(field: &str, code: &str, message: &str) -> AppError {
    AppError::validation(
        message,
        vec![Violation {
            field: field.into(),
            code: code.into(),
        }],
    )
    .with_status(422)
}

async fn profile_for_admin(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    smtp_connection_id: Uuid,
) -> Result<infrastructure::repo::smtp::SmtpBindingRow, AppError> {
    crate::marketing::revalidate_admin_session_tx(tx, ctx).await?;
    let profile = smtp::get_for_update(tx, smtp_connection_id)
        .await?
        .ok_or_else(|| AppError::not_found("SMTP connection not found"))?;
    crate::authz::require_mailbox(
        tx,
        ctx,
        profile.mail_account_id,
        crate::authz::MailboxPerm::Send,
    )
    .await?;
    Ok(profile)
}

async fn ensure_mailbox_active(tx: &mut Tx<'_>, mail_account_id: Uuid) -> Result<(), AppError> {
    let account = mailboxes::get_mail_account_scoped(tx, mail_account_id)
        .await?
        .ok_or_else(|| AppError::not_found("mail account not found"))?;
    if account.status != "active" {
        return Err(AppError::conflict("mail account is not active"));
    }
    Ok(())
}

async fn ensure_existing_secret(
    config: &Config,
    profile: &infrastructure::repo::smtp::SmtpBindingRow,
) -> Result<(), AppError> {
    if !profile.has_password
        || profile.status == "disabled"
        || profile.status == "reauth_required"
        || profile.integration_status == "disabled"
        || profile.integration_status == "reauth_required"
    {
        return Err(AppError::conflict(
            "SMTP profile has no usable stored credential",
        ));
    }
    let store = secret_store(config)?;
    if store.get(&profile.secret_ref).await?.is_none() {
        return Err(AppError::conflict(
            "SMTP profile credential must be configured again",
        ));
    }
    Ok(())
}

fn status_default(smtp_connection_id: Uuid, mail_account_id: Uuid) -> InboxSyncStatus {
    InboxSyncStatus {
        smtp_connection_id,
        mail_account_id,
        enabled: false,
        imap_host: String::new(),
        imap_port: imap_reply::IMAP_PORT,
        folder: imap_reply::INBOX_FOLDER.into(),
        cursor_uidvalidity: None,
        cursor_uid: 0,
        imported_messages: 0,
        last_success_at: None,
        last_error_code: None,
        next_sync_at: None,
        active_job_id: None,
    }
}

async fn map_status(
    tx: &mut Tx<'_>,
    smtp_connection_id: Uuid,
    mail_account_id: Uuid,
    row: Option<repo::SmtpInboxMonitorRow>,
) -> Result<InboxSyncStatus, AppError> {
    let Some(row) = row else {
        return Ok(status_default(smtp_connection_id, mail_account_id));
    };
    let active_job_id = match row.active_job_id {
        Some(id) if repo::active_job_exists(tx, id).await? => Some(id),
        _ => None,
    };
    let scheduled =
        matches!(row.scheduled_job_id, Some(id) if repo::active_job_exists(tx, id).await?);
    let cursor_uidvalidity = row
        .cursor_uidvalidity
        .map(u32::try_from)
        .transpose()
        .map_err(|_| AppError::internal("IMAP UIDVALIDITY is invalid"))?;
    let cursor_uid = u32::try_from(row.cursor_uid)
        .map_err(|_| AppError::internal("IMAP UID cursor is invalid"))?;
    Ok(InboxSyncStatus {
        smtp_connection_id,
        mail_account_id,
        enabled: row.enabled,
        imap_host: row.imap_host,
        imap_port: imap_reply::IMAP_PORT,
        folder: imap_reply::INBOX_FOLDER.into(),
        cursor_uidvalidity,
        cursor_uid,
        imported_messages: u64::try_from(row.imported_messages)
            .map_err(|_| AppError::internal("IMAP import count is invalid"))?,
        last_success_at: row.last_success_at,
        last_error_code: row.last_error_code,
        next_sync_at: scheduled.then_some(row.next_sync_at).flatten(),
        active_job_id,
    })
}

/// GET `/smtp-connections/{id}/inbox`; passwords and secret references never
/// appear in this view.
pub async fn status(
    db: &Db,
    ctx: &Ctx,
    smtp_connection_id: Uuid,
) -> Result<InboxSyncStatus, AppError> {
    crate::smtp::authorize(db, ctx, "smtp_inbox.status").await?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let profile = profile_for_admin(&mut tx, ctx, smtp_connection_id).await?;
    let row = repo::get_for_connection(&mut tx, smtp_connection_id).await?;
    let status = map_status(&mut tx, smtp_connection_id, profile.mail_account_id, row).await?;
    tx.commit().await?;
    Ok(status)
}

/// PUT `/smtp-connections/{id}/inbox`, called inside the route idempotency
/// transaction. Port and folder are server-fixed.
pub async fn set_settings_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    config: &Config,
    smtp_connection_id: Uuid,
    enabled: bool,
    requested_host: &str,
) -> Result<InboxSyncStatus, AppError> {
    let profile = profile_for_admin(tx, ctx, smtp_connection_id).await?;
    let existing = repo::get_for_connection_for_update(tx, smtp_connection_id).await?;
    let host = if requested_host.trim().is_empty() && !enabled {
        existing
            .as_ref()
            .map(|row| row.imap_host.clone())
            .unwrap_or_default()
    } else {
        requested_host.to_string()
    };

    if !host.is_empty() && !imap_reply::valid_imap_hostname(&host) {
        return Err(validation(
            "imap_host",
            "invalid_public_hostname",
            "IMAP host must be a public DNS hostname",
        ));
    }
    if enabled {
        ensure_mailbox_active(tx, profile.mail_account_id).await?;
        ensure_existing_secret(config, &profile).await?;
        match imap_reply::resolve_public_imap_addresses(&host).await {
            Ok(_) => {}
            Err(ProviderError::NotConfigured) => {
                return Err(validation(
                    "imap_host",
                    "non_public_address",
                    "IMAP host must resolve only to public addresses",
                ));
            }
            Err(_) => {
                return Err(AppError::dependency_unavailable(
                    "IMAP host could not be resolved",
                ));
            }
        }
    }

    let host_changed = existing
        .as_ref()
        .is_none_or(|row| row.imap_host.as_str() != host.as_str());
    let enabled_changed = existing.as_ref().is_none_or(|row| row.enabled != enabled);

    let monitor = if !enabled && existing.is_none() && host.is_empty() {
        None
    } else if host_changed || enabled_changed {
        if let Some(row) = &existing {
            repo::cancel_pending_jobs(tx, row.id, "SMTP_INBOX_SETTINGS_CHANGED").await?;
        }
        Some(
            repo::save_configuration(
                tx,
                smtp_connection_id,
                profile.mail_account_id,
                &host,
                enabled,
                host_changed,
            )
            .await?,
        )
    } else {
        existing
    };

    let mut monitor = monitor;
    if enabled {
        ensure_mailbox_active(tx, profile.mail_account_id).await?;
        let current_active = match monitor.as_ref().and_then(|row| row.active_job_id) {
            Some(id) if repo::active_job_exists(tx, id).await? => Some(id),
            _ => None,
        };
        let current_scheduled = match monitor.as_ref().and_then(|row| row.scheduled_job_id) {
            Some(id) if repo::active_job_exists(tx, id).await? => Some(id),
            _ => None,
        };
        if current_active.is_none()
            && current_scheduled.is_none()
            && let Some(row) = &monitor
        {
            repo::cancel_pending_jobs(tx, row.id, "SMTP_INBOX_MANUAL_SYNC").await?;
            let job_id = enqueue_poll(tx, row.id, false, Utc::now(), "settings").await?;
            repo::set_active_job(tx, row.id, Some(job_id)).await?;
            monitor = repo::get_for_connection(tx, smtp_connection_id).await?;
        }
    } else if let Some(row) = &monitor {
        repo::cancel_pending_jobs(tx, row.id, "SMTP_INBOX_DISABLED").await?;
        repo::disable_monitor(tx, row.id, None).await?;
        monitor = repo::get_for_connection(tx, smtp_connection_id).await?;
    }

    let action = if enabled {
        "smtp_inbox.enabled"
    } else {
        "smtp_inbox.disabled"
    };
    audit::record(
        tx,
        AuditEntry {
            metadata: json!({
                "smtp_connection_id": smtp_connection_id,
                "mail_account_id": profile.mail_account_id,
                "enabled": enabled,
                "imap_host": host,
                "imap_port": imap_reply::IMAP_PORT,
                "folder": imap_reply::INBOX_FOLDER,
                "credential_source": "existing_smtp_secret"
            }),
            ..AuditEntry::new(
                ctx.principal_id,
                action,
                "smtp_connection",
                ctx.request_id.clone(),
            )
            .with_target(smtp_connection_id)
        },
    )
    .await?;

    map_status(tx, smtp_connection_id, profile.mail_account_id, monitor).await
}

/// POST `/smtp-connections/{id}/inbox/sync`, called inside the route
/// idempotency transaction. At most one manual/backfill job is active per
/// monitor; repeated requests return its durable ID.
pub async fn request_sync_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    config: &Config,
    smtp_connection_id: Uuid,
) -> Result<InboxSyncQueued, AppError> {
    let profile = profile_for_admin(tx, ctx, smtp_connection_id).await?;
    ensure_mailbox_active(tx, profile.mail_account_id).await?;
    ensure_existing_secret(config, &profile).await?;
    let Some(row) = repo::get_for_connection_for_update(tx, smtp_connection_id).await? else {
        return Err(AppError::conflict("enable IMAP inbox sync first"));
    };
    if !row.enabled {
        return Err(AppError::conflict("enable IMAP inbox sync first"));
    }
    if let Some(id) = row.active_job_id
        && repo::active_job_exists(tx, id).await?
    {
        return Ok(InboxSyncQueued {
            job_id: id,
            state: "queued".into(),
        });
    }

    if let Some(scheduled) = row.scheduled_job_id {
        repo::cancel_pending_jobs(tx, row.id, "SMTP_INBOX_MANUAL_SYNC").await?;
        let _ = scheduled;
    }
    let job_id = enqueue_poll(tx, row.id, false, Utc::now(), "manual").await?;
    repo::set_active_job(tx, row.id, Some(job_id)).await?;
    audit::record(
        tx,
        AuditEntry {
            metadata: json!({
                "smtp_connection_id": smtp_connection_id,
                "mail_account_id": profile.mail_account_id,
                "folder": imap_reply::INBOX_FOLDER
            }),
            ..AuditEntry::new(
                ctx.principal_id,
                "smtp_inbox.sync_requested",
                "smtp_inbox_monitor",
                ctx.request_id.clone(),
            )
            .with_target(row.id)
        },
    )
    .await?;
    Ok(InboxSyncQueued {
        job_id,
        state: "queued".into(),
    })
}

async fn enqueue_poll(
    tx: &mut Tx<'_>,
    monitor_id: Uuid,
    scheduled: bool,
    available_at: DateTime<Utc>,
    reason: &str,
) -> Result<Uuid, AppError> {
    let dedupe_key = format!("smtp-inbox:{monitor_id}:{}", Uuid::new_v4());
    jobs::enqueue(
        tx,
        POLL_KIND,
        1,
        json!({
            "monitor_id": monitor_id,
            "scheduled": scheduled,
            "reason": reason
        }),
        available_at,
        Some(&dedupe_key),
    )
    .await
}

fn scoped_to_mailbox(
    message: &infrastructure::providers::ProviderMessage,
    mail_account_id: Uuid,
) -> bool {
    message
        .provider_message_id
        .starts_with(&format!("imap:{mail_account_id}:"))
}

fn poll_error_code(error: &ProviderError) -> (&'static str, ChronoDuration) {
    match error {
        ProviderError::NotConfigured => ("IMAP_HOST_NOT_PUBLIC", FAILURE_RETRY),
        ProviderError::ReauthRequired => ("IMAP_AUTH_REJECTED", FAILURE_RETRY),
        ProviderError::RateLimited {
            retry_after_seconds,
        } => (
            "IMAP_RATE_LIMITED",
            ChronoDuration::seconds(i64::from((*retry_after_seconds).clamp(60, 3600))),
        ),
        ProviderError::Unavailable(_) => ("IMAP_POLL_FAILED", ChronoDuration::minutes(2)),
    }
}

/// One bounded `smtp_inbox.poll` worker invocation. The monitor lock is held
/// across the page read and inbox ingest so a duplicate lease cannot advance
/// or replay a different cursor concurrently. Cursor and messages commit in
/// the same fenced job transaction.
pub async fn poll_page(
    tx: &mut Tx<'_>,
    job_id: Uuid,
    monitor_id: Uuid,
    scheduled: bool,
    config: &Config,
    source: &dyn ImapInboxSource,
) -> Result<(), AppError> {
    let Some(snapshot) = repo::get_for_id(tx, monitor_id).await? else {
        return Ok(());
    };
    let Some(monitor) = repo::get_for_id_for_update(tx, monitor_id).await? else {
        return Ok(());
    };
    if !monitor.enabled {
        return Ok(());
    }
    if scheduled {
        if monitor.scheduled_job_id != Some(job_id) {
            return Ok(());
        }
    } else if monitor.active_job_id != Some(job_id) {
        return Ok(());
    }
    let profile = repo::profile_for_sync(tx, snapshot.smtp_connection_id)
        .await?
        .ok_or_else(|| AppError::not_found("SMTP connection not found"))?;

    // SMTP profile deactivation also stops IMAP polling. The shared profile
    // is read without an update lock: worker_runtime is intentionally granted
    // SELECT only on SMTP profiles. The admin profile-disable trigger updates
    // the locked monitor row, so it waits for an already-running page to
    // commit and makes later pages observe the disabled state.
    if profile.status == "disabled"
        || profile.status == "reauth_required"
        || profile.integration_status == "disabled"
        || profile.integration_status == "reauth_required"
        || !profile.has_password
    {
        repo::cancel_pending_jobs(tx, monitor.id, "SMTP_PROFILE_DISABLED").await?;
        repo::disable_monitor(tx, monitor.id, Some("SMTP_PROFILE_DISABLED")).await?;
        audit_poll(
            tx,
            job_id,
            monitor.id,
            "smtp_inbox.disabled_profile",
            json!({"smtp_connection_id": profile.id}),
        )
        .await?;
        return Ok(());
    }
    if let Some(account) = mailboxes::get_mail_account_scoped(tx, profile.mail_account_id).await?
        && account.status != "active"
    {
        repo::cancel_pending_jobs(tx, monitor.id, "MAILBOX_INACTIVE").await?;
        repo::disable_monitor(tx, monitor.id, Some("MAILBOX_INACTIVE")).await?;
        audit_poll(
            tx,
            job_id,
            monitor.id,
            "smtp_inbox.disabled_mailbox",
            json!({"mail_account_id": profile.mail_account_id}),
        )
        .await?;
        return Ok(());
    }

    repo::mark_poll_started(tx, monitor.id, job_id, scheduled).await?;
    let store = secret_store(config)?;
    let password = match store.get(&profile.secret_ref).await? {
        Some(password) => SecretValue::new(password),
        None => {
            repo::cancel_pending_jobs(tx, monitor.id, "SMTP_CREDENTIAL_MISSING").await?;
            repo::disable_monitor(tx, monitor.id, Some("SMTP_CREDENTIAL_MISSING")).await?;
            audit_poll(
                tx,
                job_id,
                monitor.id,
                "smtp_inbox.credential_missing",
                json!({"smtp_connection_id": profile.id}),
            )
            .await?;
            return Ok(());
        }
    };
    let cursor = imap_reply::ImapCursor {
        uid_validity: monitor
            .cursor_uidvalidity
            .map(u32::try_from)
            .transpose()
            .map_err(|_| AppError::internal("IMAP UIDVALIDITY is invalid"))?,
        last_uid: u32::try_from(monitor.cursor_uid)
            .map_err(|_| AppError::internal("IMAP UID cursor is invalid"))?,
    };
    let page = match source
        .fetch_inbox_page(
            &monitor.imap_host,
            &profile.username,
            &password,
            profile.mail_account_id,
            cursor,
        )
        .await
    {
        Ok(page) => page,
        Err(error) => {
            let (code, delay) = poll_error_code(&error);
            let at = Utc::now() + delay;
            let scheduled_job = enqueue_poll(tx, monitor.id, true, at, "retry").await?;
            repo::fail_poll(tx, monitor.id, code, Some(scheduled_job), Some(at)).await?;
            audit_poll(
                tx,
                job_id,
                monitor.id,
                "smtp_inbox.poll_failed",
                json!({"error_code": code, "retry_at": at}),
            )
            .await?;
            return Ok(());
        }
    };

    let mut imported = 0usize;
    for message in &page.messages {
        if !scoped_to_mailbox(message, profile.mail_account_id) {
            return Err(AppError::internal(
                "IMAP source returned a message for another mailbox",
            ));
        }
        let outcome = crate::inbox::ingest::ingest_provider_message(
            tx,
            Some(job_id),
            profile.mail_account_id,
            message,
        )
        .await?;
        if outcome.message_id.is_some() {
            imported += 1;
        }
    }

    let (active_job_id, scheduled_job_id, next_sync_at) = if page.has_more {
        let at = Utc::now() + CONTINUATION_DELAY;
        let next = enqueue_poll(tx, monitor.id, false, at, "backfill").await?;
        (Some(next), None, None)
    } else {
        let at = Utc::now() + POLL_INTERVAL;
        let next = enqueue_poll(tx, monitor.id, true, at, "periodic").await?;
        (None, Some(next), Some(at))
    };
    repo::complete_poll(
        tx,
        monitor.id,
        page.uid_validity,
        page.last_uid,
        imported,
        active_job_id,
        scheduled_job_id,
        next_sync_at,
    )
    .await?;
    audit_poll(
        tx,
        job_id,
        monitor.id,
        "smtp_inbox.synced",
        json!({
            "smtp_connection_id": profile.id,
            "mail_account_id": profile.mail_account_id,
            "folder": imap_reply::INBOX_FOLDER,
            "uidvalidity": page.uid_validity,
            "last_uid": page.last_uid,
            "scanned": page.scanned,
            "imported": imported,
            "skipped": page.skipped,
            "truncated": page.truncated,
            "has_more": page.has_more
        }),
    )
    .await?;
    Ok(())
}

async fn audit_poll(
    tx: &mut Tx<'_>,
    job_id: Uuid,
    monitor_id: Uuid,
    action: &str,
    metadata: serde_json::Value,
) -> Result<(), AppError> {
    audit::record(
        tx,
        AuditEntry {
            actor_principal_id: None,
            requested_by: None,
            approved_by: None,
            job_id: Some(job_id),
            action: action.into(),
            target_type: "smtp_inbox_monitor".into(),
            target_id: Some(monitor_id),
            outcome: "ok".into(),
            metadata,
            request_id: job_id.to_string(),
            trace_id: None,
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::scoped_to_mailbox;
    use chrono::Utc;
    use infrastructure::providers::ProviderMessage;
    use uuid::Uuid;

    fn message(provider_message_id: String, from: &str) -> ProviderMessage {
        ProviderMessage {
            provider_message_id,
            provider_thread_id: None,
            internet_message_id: None,
            in_reply_to: None,
            references: Vec::new(),
            from: from.into(),
            to: vec!["office@example.test".into()],
            subject: None,
            body_text: None,
            body_html: None,
            message_at: Utc::now(),
        }
    }

    #[test]
    fn full_inbox_import_keeps_unrelated_senders_but_rejects_foreign_mailbox_ids() {
        let mailbox_a = Uuid::from_u128(1);
        let mailbox_b = Uuid::from_u128(2);
        let vendor_reply = message(
            format!("imap:{mailbox_a}:fingerprint:99:8"),
            "ap@example.net",
        );
        let foreign_mail = message(
            format!("imap:{mailbox_b}:fingerprint:99:8"),
            "ap@example.net",
        );

        assert!(scoped_to_mailbox(&vendor_reply, mailbox_a));
        assert!(!scoped_to_mailbox(&foreign_mail, mailbox_a));
    }
}
