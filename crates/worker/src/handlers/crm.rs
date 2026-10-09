//! crm module job handlers (spec §6 — owned by the module worker).
//!
//! - `crm.merge_companies` — applies a merge request (application::crm::merge).
//!   Internal-only work; `external_effect = false`.
//! - `crm.activity_projector` — EventConsumer projecting the six CRM-relevant
//!   outbox events onto the `activities` timeline (§5.6 grant-filtered reads).

use application::crm::{merge, projector};
use async_trait::async_trait;
use contracts::AppError;
use infrastructure::db::Tx;
use infrastructure::jobs::{JobCtx, JobHandler};
use infrastructure::outbox::OutboxRow;

use crate::JobRegistry;
use crate::consumers::{EventConsumer, EventCtx};

/// Applies the merge payload `{merge_request: {source_id, target_id,
/// field_choices}, requested_by}` (integration contracts).
struct MergeCompanies;

#[async_trait]
impl JobHandler for MergeCompanies {
    fn kind(&self) -> &'static str {
        "crm.merge_companies"
    }

    async fn handle(
        &self,
        ctx: &JobCtx,
        tx: &mut Tx<'_>,
        payload: serde_json::Value,
    ) -> Result<(), AppError> {
        merge::apply(tx, ctx.job_id, payload).await
    }
}

/// Writes `activities` rows from outbox events. All dedupe is anchored by
/// `source_event_id = event.id`, so re-delivery is a no-op.
struct ActivityProjector;

#[async_trait]
impl EventConsumer for ActivityProjector {
    fn name(&self) -> &'static str {
        "crm.activity_projector"
    }

    fn event_types(&self) -> &'static [&'static str] {
        projector::EVENT_TYPES
    }

    async fn handle(
        &self,
        _ctx: &EventCtx,
        tx: &mut Tx<'_>,
        event: &OutboxRow,
    ) -> Result<(), AppError> {
        projector::handle(tx, event).await
    }
}

/// Register this module's JobHandlers and EventConsumers on the registry.
pub fn register(registry: &mut JobRegistry) {
    registry.register(MergeCompanies);
    registry.register_consumer(ActivityProjector);
}
