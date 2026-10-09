//! `inbox.sync_mailbox` and `inbox.process_webhook` use-case bodies
//! (§9.1 overlap re-scan + upsert, §14.3). The worker handlers own the
//! job plumbing; these functions do the work inside the job's tx.
//!
//! The provider call happens inside the tx — fine for the in-memory fake;
//! a slow real provider would hold the tx briefly (acceptable for v1.0
//! since per-mailbox sync is serialized by the mailbox row lock).

use chrono::Utc;
use contracts::AppError;
use serde_json::json;
use uuid::Uuid;

use super::ingest::ingest_provider_message;
use infrastructure::db::Tx;
use infrastructure::providers::{MailProvider, ProviderError};
use infrastructure::repo::inbox as repo;
use infrastructure::{audit, audit::AuditEntry};

const STREAM_KEY: &str = "inbound";
const PAGE_LIMIT: u32 = 200;
const MAX_PAGES: u32 = 25;
/// Periodic re-scan cadence (§9.1 "주기적 보정" — webhook loss/out-of-order
/// recovery). Self-rescheduling keeps at most one pending periodic job per
/// mailbox via the time-bucketed dedupe key.
const PERIODIC_RESYNC_SECS: i64 = 900;

pub struct SyncOutcome {
    pub scanned: usize,
    pub inserted: usize,
}

/// `inbox.sync_mailbox {mail_account_id, reason}` — overlap re-scan +
/// upsert by provider message id.
pub async fn sync_mailbox(
    tx: &mut Tx<'_>,
    mail: &dyn MailProvider,
    job_id: Uuid,
    mail_account_id: Uuid,
    reason: &str,
) -> Result<SyncOutcome, AppError> {
    let Some(account) = repo::get_mail_account(tx, mail_account_id).await? else {
        // Account removed — nothing to do; complete the job.
        return Ok(SyncOutcome {
            scanned: 0,
            inserted: 0,
        });
    };
    if account.status != "active" {
        return Ok(SyncOutcome {
            scanned: 0,
            inserted: 0,
        });
    }

    let cursor_data = repo::get_sync_cursor(tx, mail_account_id, STREAM_KEY).await?;
    let mut cursor = cursor_data
        .as_ref()
        .and_then(|c| c.get("next"))
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let mut scanned = 0usize;
    let mut inserted = 0usize;
    let mut messages = Vec::new();
    for _ in 0..(MAX_PAGES as usize).min(mail.sync_page_budget().max(1)) {
        let (page, next) = mail
            .list_messages(cursor.clone(), PAGE_LIMIT)
            .await
            .map_err(provider_error)?;
        scanned += page.len();
        messages.extend(page);
        match next {
            Some(n) => cursor = Some(n),
            None => break,
        }
    }
    // Deliver in message_at order so the thread's last-inbound pointer and
    // read marks settle deterministically under out-of-order delivery.
    messages.sort_by_key(|m| m.message_at);

    for msg in &messages {
        let out = ingest_provider_message(tx, Some(job_id), mail_account_id, msg).await?;
        if out.message_id.is_some() {
            inserted += 1;
        }
    }

    repo::upsert_sync_cursor(
        tx,
        mail_account_id,
        STREAM_KEY,
        &json!({
            "next": cursor,
            "synced_at": Utc::now(),
            "reason": reason,
        }),
        true,
        None,
    )
    .await?;
    repo::touch_mail_account_synced(tx, mail_account_id).await?;

    // Periodic correction: one pending re-scan per mailbox per window.
    let bucket = Utc::now().timestamp() / PERIODIC_RESYNC_SECS;
    repo::enqueue_coalesced_at(
        tx,
        "inbox.sync_mailbox",
        &json!({"mail_account_id": mail_account_id, "reason": "periodic"}),
        &format!("sync:{mail_account_id}:p{bucket}"),
        Utc::now() + chrono::Duration::seconds(PERIODIC_RESYNC_SECS),
    )
    .await?;

    audit::record(
        tx,
        AuditEntry {
            actor_principal_id: None,
            requested_by: None,
            approved_by: None,
            job_id: Some(job_id),
            action: "mailbox.synced".into(),
            target_type: "mail_account".into(),
            target_id: Some(mail_account_id),
            outcome: "ok".into(),
            metadata: json!({"scanned": scanned, "inserted": inserted, "reason": reason}),
            request_id: job_id.to_string(),
            trace_id: None,
        },
    )
    .await?;

    Ok(SyncOutcome { scanned, inserted })
}

/// `inbox.process_webhook {webhook_event_id}` — targeted ingest of the
/// event's provider message (when the payload carries an id), then a
/// coalesced follow-up sync for whatever the event didn't cover.
pub async fn process_webhook(
    tx: &mut Tx<'_>,
    mail: &dyn MailProvider,
    job_id: Uuid,
    webhook_event_id: Uuid,
) -> Result<(), AppError> {
    let Some(event) = repo::get_webhook_event(tx, webhook_event_id).await? else {
        return Ok(()); // row gone — complete
    };
    let Some(account) = repo::mail_account_for_connection(tx, event.connection_id).await? else {
        repo::mark_webhook_event(tx, webhook_event_id, "failed").await?;
        return Ok(());
    };

    if let Some(pmid) = event
        .redacted_payload
        .get("provider_message_id")
        .and_then(|v| v.as_str())
    {
        match mail.get_message(pmid).await {
            Ok(msg) => {
                ingest_provider_message(tx, Some(job_id), account.id, &msg).await?;
            }
            Err(ProviderError::Unavailable(e)) => {
                // The message may already be purged provider-side; the
                // follow-up sync reconciles. Don't fail the durable event.
                tracing::warn!(error = %e, "webhook message fetch failed");
            }
            Err(e) => {
                repo::mark_webhook_event(tx, webhook_event_id, "failed").await?;
                return Err(provider_error(e));
            }
        }
    }

    // Coalesced drift recovery — every webhook also schedules a re-scan.
    repo::enqueue_coalesced(
        tx,
        "inbox.sync_mailbox",
        &json!({"mail_account_id": account.id, "reason": "webhook"}),
        &format!("sync:{}:webhook", account.id),
    )
    .await?;

    repo::mark_webhook_event(tx, webhook_event_id, "processed").await?;
    Ok(())
}

fn provider_error(e: ProviderError) -> AppError {
    match e {
        ProviderError::NotConfigured => AppError::new(
            contracts::ErrorCode::IntegrationReauthRequired,
            "mail provider is not configured",
        ),
        ProviderError::ReauthRequired => AppError::new(
            contracts::ErrorCode::IntegrationReauthRequired,
            "mail provider requires re-authentication",
        ),
        ProviderError::RateLimited {
            retry_after_seconds,
        } => AppError::new(contracts::ErrorCode::RateLimited, "provider rate limited")
            .with_retry_after(retry_after_seconds),
        ProviderError::Unavailable(m) => {
            AppError::dependency_unavailable(format!("mail provider: {m}"))
        }
    }
}
