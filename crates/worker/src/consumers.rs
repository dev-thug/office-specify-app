//! Outbox event consumers (spec §4.7 dispatcher, integration-contracts
//! "Outbox events"). Parallel to `JobHandler`: a module implements
//! `EventConsumer`, registers it via its `handlers::<m>::register(&mut
//! JobRegistry)`, and the `Runner`'s outbox tick delivers each unpublished
//! event to every subscribed consumer — each in its own transaction with a
//! `processed_events` dedupe row inserted first, so re-delivery is
//! effectively-once for DB effects.

use async_trait::async_trait;
use contracts::AppError;
use infrastructure::db::Tx;
use infrastructure::outbox::OutboxRow;
use uuid::Uuid;

/// Context handed to a consumer — parallel to `JobCtx`.
#[derive(Debug, Clone)]
pub struct EventCtx {
    pub event_id: Uuid,
    pub workspace_id: Uuid,
    /// Consumer name as stored in `processed_events.consumer`.
    pub consumer: &'static str,
    /// Unique owner of the running dispatcher (lease-owner analog).
    pub dispatcher: String,
}

#[async_trait]
pub trait EventConsumer: Send + Sync {
    /// Stable consumer key stored in `processed_events.consumer` (dedupe
    /// anchor — renaming creates a new consumer).
    fn name(&self) -> &'static str;
    /// Event types this consumer subscribes to (§16.2 / integration doc).
    fn event_types(&self) -> &'static [&'static str];
    /// Runs inside the same tx that holds the consumer's `processed_events`
    /// row. Returning `Err` rolls back both the row and any business writes,
    /// so the event is retried on a later tick.
    async fn handle(
        &self,
        ctx: &EventCtx,
        tx: &mut Tx<'_>,
        event: &OutboxRow,
    ) -> Result<(), AppError>;
}
