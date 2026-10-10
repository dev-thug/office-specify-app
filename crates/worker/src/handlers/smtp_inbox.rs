//! Bounded IMAP INBOX page jobs for an explicitly enabled SMTP profile.

use std::sync::Arc;

use contracts::AppError;
use infrastructure::{
    config::Config,
    db::Tx,
    jobs::{JobCtx, JobHandler},
    providers::imap_reply::{ImapInboxSource, TlsImapInboxSource},
};
use serde_json::Value;
use uuid::Uuid;

use crate::{JobRegistry, WorkerContext};

struct PollInbox {
    config: Option<Arc<Config>>,
    source: Arc<dyn ImapInboxSource>,
}

#[async_trait::async_trait]
impl JobHandler for PollInbox {
    fn kind(&self) -> &'static str {
        "smtp_inbox.poll"
    }

    async fn handle(&self, ctx: &JobCtx, tx: &mut Tx<'_>, payload: Value) -> Result<(), AppError> {
        let monitor_id = payload
            .get("monitor_id")
            .and_then(Value::as_str)
            .and_then(|value| Uuid::parse_str(value).ok())
            .ok_or_else(|| AppError::validation("job payload missing monitor_id", vec![]))?;
        let scheduled = payload
            .get("scheduled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let config = self
            .config
            .as_deref()
            .ok_or_else(|| AppError::internal("SMTP inbox handler requires WorkerContext"))?;
        application::smtp_inbox::poll_page(
            tx,
            ctx.job_id,
            monitor_id,
            scheduled,
            config,
            self.source.as_ref(),
        )
        .await
    }
}

/// Registry-only registration remains available to tests that inspect job
/// kinds without executing provider-backed work.
pub fn register(registry: &mut JobRegistry) {
    registry.register(PollInbox {
        config: None,
        source: Arc::new(TlsImapInboxSource),
    });
}

pub fn register_with_context(registry: &mut JobRegistry, context: &WorkerContext) {
    register_with_source(
        registry,
        Arc::new(context.config.as_ref().clone()),
        Arc::new(TlsImapInboxSource),
    );
}

/// Explicit provider seam for worker tests; production uses the TLS adapter
/// through `register_with_context`.
pub fn register_with_source(
    registry: &mut JobRegistry,
    config: Arc<Config>,
    source: Arc<dyn ImapInboxSource>,
) {
    registry.register(PollInbox {
        config: Some(config),
        source,
    });
}
