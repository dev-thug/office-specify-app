//! Sending module job handlers.
//!
//! Dispatch commits the submitting boundary before provider I/O. Reconcile
//! uses provider lookup only and never resubmits an unknown outbound.

use std::sync::Arc;

use contracts::AppError;
use infrastructure::db::Db;
use infrastructure::jobs::{JobCtx, JobHandler};
use uuid::Uuid;

use crate::{JobRegistry, WorkerContext};

/// Dependencies used by the two sending jobs.
pub struct SendingDeps {
    pub db: Db,
    pub providers: application::sending::dispatch::Providers,
}

fn outbound_id(payload: &serde_json::Value) -> Option<Uuid> {
    payload
        .get("outbound_message_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
}

struct DispatchOutbound {
    deps: Option<Arc<SendingDeps>>,
}

#[async_trait::async_trait]
impl JobHandler for DispatchOutbound {
    fn kind(&self) -> &'static str {
        application::sending::dispatch::DISPATCH_KIND
    }

    fn external_effect(&self) -> bool {
        true
    }

    async fn handle(
        &self,
        ctx: &JobCtx,
        tx: &mut infrastructure::db::Tx<'_>,
        payload: serde_json::Value,
    ) -> Result<(), AppError> {
        let _ = tx;
        let Some(id) = outbound_id(&payload) else {
            tracing::warn!(job_id = %ctx.job_id, "dispatch payload missing outbound_message_id");
            return Ok(());
        };
        let deps = self
            .deps
            .as_deref()
            .ok_or_else(|| AppError::internal("sending handler requires WorkerContext"))?;
        application::sending::dispatch::run_dispatch(&deps.db, &deps.providers, ctx, id).await
    }
}

struct ReconcileUnknown {
    deps: Option<Arc<SendingDeps>>,
}

#[async_trait::async_trait]
impl JobHandler for ReconcileUnknown {
    fn kind(&self) -> &'static str {
        application::sending::dispatch::RECONCILE_KIND
    }

    /// Provider reads are safe to retry; this job never resubmits mail.
    fn external_effect(&self) -> bool {
        false
    }

    async fn handle(
        &self,
        ctx: &JobCtx,
        tx: &mut infrastructure::db::Tx<'_>,
        payload: serde_json::Value,
    ) -> Result<(), AppError> {
        let _ = tx;
        let Some(id) = outbound_id(&payload) else {
            tracing::warn!(job_id = %ctx.job_id, "reconcile payload missing outbound_message_id");
            return Ok(());
        };
        let deps = self
            .deps
            .as_deref()
            .ok_or_else(|| AppError::internal("sending handler requires WorkerContext"))?;
        application::sending::dispatch::run_reconcile(&deps.db, &deps.providers, ctx, id).await
    }
}

/// Context-free registration remains available to registry-only tests.
pub fn register(registry: &mut JobRegistry) {
    registry.register(DispatchOutbound { deps: None });
    registry.register(ReconcileUnknown { deps: None });
}

/// Register with the per-Runner database and provider adapters.
pub fn register_with_context(registry: &mut JobRegistry, context: &WorkerContext) {
    let deps = Arc::new(SendingDeps {
        db: context.db.clone(),
        providers: application::sending::dispatch::Providers {
            mail: context.mail.clone(),
            delivery: context.delivery.clone(),
        },
    });
    registry.register(DispatchOutbound {
        deps: Some(deps.clone()),
    });
    registry.register(ReconcileUnknown { deps: Some(deps) });
}
