//! Inbox sync and webhook processing jobs. Both use provider reads and
//! upsert by mailbox/provider message ID to make replay safe.

use std::sync::Arc;

use contracts::AppError;
use infrastructure::db::Tx;
use infrastructure::jobs::{JobCtx, JobHandler};
use infrastructure::providers::MailProvider;
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
        let mail = self
            .mail
            .as_deref()
            .ok_or_else(|| AppError::internal("inbox handler requires WorkerContext"))?;
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
        let mail = self
            .mail
            .as_deref()
            .ok_or_else(|| AppError::internal("inbox handler requires WorkerContext"))?;
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
