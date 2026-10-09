//! compliance module job handlers (spec §6 — owned by the module worker).
//!
//! `compliance.release_suppression` — enqueued by the approvals module on
//! an approved `suppression_release` decision (integration contract). The
//! handler re-verifies the approval inside its own transaction and runs
//! `compliance_core::release_suppression`. No `external_effect`: it only
//! writes DB state, so runner retries are safe and idempotent.

use async_trait::async_trait;
use contracts::AppError;
use infrastructure::db::Tx;
use infrastructure::jobs::{JobCtx, JobHandler};

use crate::JobRegistry;

struct ReleaseSuppressionJob;

#[async_trait]
impl JobHandler for ReleaseSuppressionJob {
    fn kind(&self) -> &'static str {
        "compliance.release_suppression"
    }

    async fn handle(
        &self,
        ctx: &JobCtx,
        tx: &mut Tx<'_>,
        payload: serde_json::Value,
    ) -> Result<(), AppError> {
        application::compliance::release::handle(tx, ctx.job_id, &payload).await
    }
}

/// Register this module's JobHandlers on the worker registry.
pub fn register(registry: &mut JobRegistry) {
    registry.register(ReleaseSuppressionJob);
}
