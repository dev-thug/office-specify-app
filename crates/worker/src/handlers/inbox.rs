//! Inbox sync and webhook processing jobs. Both use provider reads and
//! upsert by mailbox/provider message ID to make replay safe.

use std::sync::Arc;

use contracts::AppError;
use infrastructure::db::Tx;
use infrastructure::jobs::{JobCtx, JobHandler};
use infrastructure::providers::{MailProvider, ProviderError};
use infrastructure::repo::inbox as inbox_repo;
use uuid::Uuid;

use crate::{JobRegistry, WorkerContext};

fn payload_uuid(payload: &serde_json::Value, key: &str) -> Result<Uuid, AppError> {
    payload
        .get(key)
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| AppError::validation(format!("job payload missing {key}"), vec![]))
}

struct SyncMailbox {
    mail: Option<Arc<dyn MailProvider>>,
}

#[async_trait::async_trait]
impl JobHandler for SyncMailbox {
    fn kind(&self) -> &'static str {
        "inbox.sync_mailbox"
    }

    async fn handle(
        &self,
        ctx: &JobCtx,
        tx: &mut Tx<'_>,
        payload: serde_json::Value,
    ) -> Result<(), AppError> {
        let mail_account_id = payload_uuid(&payload, "mail_account_id")?;
        let reason = payload
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("manual")
            .to_string();
        let router = self
            .mail
            .as_deref()
            .ok_or_else(|| AppError::internal("inbox handler requires WorkerContext"))?;
        let workspace_id = tx
            .workspace_id()
            .ok_or_else(|| AppError::internal("inbox sync has no workspace transaction"))?;
        let bound = router
            .for_mailbox(workspace_id, mail_account_id)
            .await
            .map_err(provider_error)?;
        let mail = bound.as_deref().unwrap_or(router);
        application::inbox::sync::sync_mailbox(tx, mail, ctx.job_id, mail_account_id, &reason)
            .await?;
        Ok(())
    }
}

struct ProcessWebhook {
    mail: Option<Arc<dyn MailProvider>>,
}

#[async_trait::async_trait]
impl JobHandler for ProcessWebhook {
    fn kind(&self) -> &'static str {
        "inbox.process_webhook"
    }

    async fn handle(
        &self,
        ctx: &JobCtx,
        tx: &mut Tx<'_>,
        payload: serde_json::Value,
    ) -> Result<(), AppError> {
        let event_id = payload_uuid(&payload, "webhook_event_id")?;
        let router = self
            .mail
            .as_deref()
            .ok_or_else(|| AppError::internal("inbox handler requires WorkerContext"))?;
        let Some(event) = inbox_repo::get_webhook_event(tx, event_id).await? else {
            return application::inbox::sync::process_webhook(tx, router, ctx.job_id, event_id)
                .await;
        };
        let Some(account) =
            inbox_repo::mail_account_for_connection(tx, event.connection_id).await?
        else {
            return application::inbox::sync::process_webhook(tx, router, ctx.job_id, event_id)
                .await;
        };
        if account.status != "active" {
            inbox_repo::mark_webhook_event(tx, event_id, "failed").await?;
            return Ok(());
        }
        let workspace_id = tx
            .workspace_id()
            .ok_or_else(|| AppError::internal("webhook job has no workspace transaction"))?;
        let bound = router
            .for_mailbox(workspace_id, account.id)
            .await
            .map_err(provider_error)?;
        let mail = bound.as_deref().unwrap_or(router);
        application::inbox::sync::process_webhook(tx, mail, ctx.job_id, event_id).await
    }
}

/// Context-free registration remains available to registry-only tests.
pub fn register(registry: &mut JobRegistry) {
    registry.register(SyncMailbox { mail: None });
    registry.register(ProcessWebhook { mail: None });
}

/// Register with the per-Runner provider adapters.
pub fn register_with_context(registry: &mut JobRegistry, context: &WorkerContext) {
    registry.register(SyncMailbox {
        mail: Some(context.mail.clone()),
    });
    registry.register(ProcessWebhook {
        mail: Some(context.mail.clone()),
    });
}

fn provider_error(error: ProviderError) -> AppError {
    match error {
        ProviderError::NotConfigured | ProviderError::ReauthRequired => AppError::new(
            contracts::ErrorCode::IntegrationReauthRequired,
            "mailbox provider is not configured or requires reconnection",
        ),
        ProviderError::RateLimited {
            retry_after_seconds,
        } => AppError::new(contracts::ErrorCode::RateLimited, "Zoho Mail rate limited")
            .with_retry_after(retry_after_seconds),
        ProviderError::Unavailable(_) => {
            AppError::dependency_unavailable("Zoho Mail is temporarily unavailable")
        }
    }
}
