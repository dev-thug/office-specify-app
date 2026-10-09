//! Booking calendar saga and hold-expiry jobs.

use async_trait::async_trait;
use contracts::AppError;
use infrastructure::db::Tx;
use infrastructure::jobs::{JobCtx, JobHandler};

use crate::{JobRegistry, WorkerContext};

struct ConfirmBookingJob(WorkerContext);

#[async_trait]
impl JobHandler for ConfirmBookingJob {
    fn kind(&self) -> &'static str {
        "booking.confirm"
    }

    fn external_effect(&self) -> bool {
        true
    }

    async fn handle(
        &self,
        ctx: &JobCtx,
        _tx: &mut Tx<'_>,
        payload: serde_json::Value,
    ) -> Result<(), AppError> {
        let booking_id = payload
            .get("booking_id")
            .and_then(serde_json::Value::as_str)
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
            .ok_or_else(|| AppError::validation("booking_id is required", vec![]))?;
        application::booking::confirm_booking(
            &self.0.db,
            self.0.calendar.as_ref(),
            ctx.workspace_id,
            booking_id,
            ctx.job_id,
        )
        .await
    }
}

struct ApplyBookingChangeJob(WorkerContext);

#[async_trait]
impl JobHandler for ApplyBookingChangeJob {
    fn kind(&self) -> &'static str {
        "booking.apply_change"
    }

    fn external_effect(&self) -> bool {
        true
    }

    async fn handle(
        &self,
        ctx: &JobCtx,
        _tx: &mut Tx<'_>,
        payload: serde_json::Value,
    ) -> Result<(), AppError> {
        let change_id = payload
            .get("booking_change_id")
            .and_then(serde_json::Value::as_str)
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
            .ok_or_else(|| AppError::validation("booking_change_id is required", vec![]))?;
        application::booking::apply_booking_change(
            &self.0.db,
            self.0.calendar.as_ref(),
            ctx.workspace_id,
            change_id,
            ctx.job_id,
        )
        .await
    }
}

struct ExpireBookingHoldsJob;

#[async_trait]
impl JobHandler for ExpireBookingHoldsJob {
    fn kind(&self) -> &'static str {
        "booking.expire_holds"
    }

    async fn handle(
        &self,
        ctx: &JobCtx,
        tx: &mut Tx<'_>,
        _payload: serde_json::Value,
    ) -> Result<(), AppError> {
        application::booking::expire_holds(tx, ctx.job_id).await
    }
}

/// Context-free registry for tests that only inspect the handler list.
/// Provider-backed jobs are wired by `register_with_context`.
pub fn register(_registry: &mut JobRegistry) {}

pub fn register_with_context(registry: &mut JobRegistry, context: &WorkerContext) {
    registry.register(ConfirmBookingJob(context.clone()));
    registry.register(ApplyBookingChangeJob(context.clone()));
    registry.register(ExpireBookingHoldsJob);
}
