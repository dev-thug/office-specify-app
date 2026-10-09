//! campaigns module job handlers + outbox consumers (spec §6 — module
//! worker owned). Job kinds per integration-contracts:
//!   campaigns.freeze_launch      → A.6.2 freeze → approval request
//!   campaigns.activate_launch    → arm step-1 sends after approval
//!   campaigns.schedule_next_step → advance an enrollment after acceptance
//!   campaigns.pause_address      → §8.4 stop-condition fan-out
//!   compliance.pause_enrollments → suppression pause (job enqueued by
//!                                  compliance_core::insert_suppression)
//! Consumers: outbound.accepted / outbound.failed / message.received /
//! booking.requested.

use std::collections::BTreeMap;

use async_trait::async_trait;
use contracts::AppError;
use infrastructure::db::Tx;
use infrastructure::jobs::{JobCtx, JobHandler};
use infrastructure::outbox::OutboxRow;
use infrastructure::repo::campaigns as repo;
use uuid::Uuid;

use crate::JobRegistry;
use crate::consumers::{EventConsumer, EventCtx};

/// Register this module's JobHandlers + event consumers on the registry.
pub fn register(registry: &mut JobRegistry) {
    registry.register(FreezeLaunch);
    registry.register(ActivateLaunch);
    registry.register(ScheduleNextStep);
    registry.register(PauseAddress);
    registry.register(PauseEnrollments);
    registry.register_consumer(CampaignEvents);
}

/// System principal for worker-side audit rows; falls back to the launch
/// requester context (job rows carry `requested_by`, but the audit actor
/// convention is the system principal).
async fn system_actor(tx: &mut Tx<'_>) -> Result<Uuid, AppError> {
    Ok(repo::system_principal_id(tx)
        .await?
        .unwrap_or_else(|| Uuid::from_u128(0)))
}

fn payload_uuid(payload: &serde_json::Value, key: &str) -> Result<Uuid, AppError> {
    payload
        .get(key)
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| AppError::validation(format!("payload.{key} missing/invalid"), vec![]))
}

// ------------------------------------------------------------- job kinds

struct FreezeLaunch;

#[async_trait]
impl JobHandler for FreezeLaunch {
    fn kind(&self) -> &'static str {
        "campaigns.freeze_launch"
    }

    async fn handle(
        &self,
        ctx: &JobCtx,
        tx: &mut Tx<'_>,
        payload: serde_json::Value,
    ) -> Result<(), AppError> {
        let launch_id = payload_uuid(&payload, "campaign_launch_id")?;
        let ids: Vec<Uuid> = payload
            .get("email_address_ids")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        let variables: BTreeMap<String, String> = payload
            .get("variables")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        let requester_context = payload
            .get("requester_context")
            .cloned()
            .ok_or_else(|| AppError::validation("payload.requester_context missing", vec![]))?;
        let actor = system_actor(tx).await?;
        application::campaigns::freeze_launch(
            tx,
            actor,
            ctx.job_id,
            launch_id,
            &ids,
            &variables,
            &requester_context,
        )
        .await
    }
}

struct ActivateLaunch;

#[async_trait]
impl JobHandler for ActivateLaunch {
    fn kind(&self) -> &'static str {
        "campaigns.activate_launch"
    }

    async fn handle(
        &self,
        ctx: &JobCtx,
        tx: &mut Tx<'_>,
        payload: serde_json::Value,
    ) -> Result<(), AppError> {
        let launch_id = payload_uuid(&payload, "campaign_launch_id")?;
        let request_id = payload_uuid(&payload, "approval_request_id")?;
        let actor = system_actor(tx).await?;
        application::campaigns::activate_launch(tx, actor, ctx.job_id, launch_id, request_id).await
    }
}

struct ScheduleNextStep;

#[async_trait]
impl JobHandler for ScheduleNextStep {
    fn kind(&self) -> &'static str {
        "campaigns.schedule_next_step"
    }

    async fn handle(
        &self,
        ctx: &JobCtx,
        tx: &mut Tx<'_>,
        payload: serde_json::Value,
    ) -> Result<(), AppError> {
        let enrollment_id = payload_uuid(&payload, "campaign_enrollment_id")?;
        let actor = system_actor(tx).await?;
        application::campaigns::schedule_next_step(tx, actor, ctx.job_id, enrollment_id).await
    }
}

struct PauseAddress;

#[async_trait]
impl JobHandler for PauseAddress {
    fn kind(&self) -> &'static str {
        "campaigns.pause_address"
    }

    async fn handle(
        &self,
        ctx: &JobCtx,
        tx: &mut Tx<'_>,
        payload: serde_json::Value,
    ) -> Result<(), AppError> {
        let addr = payload_uuid(&payload, "email_address_id")?;
        let reason = payload
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("human_reply")
            .to_string();
        let actor = system_actor(tx).await?;
        application::campaigns::pause_address(tx, actor, ctx.job_id, addr, &reason).await
    }
}

/// `compliance.pause_enrollments` — emitted transactionally by
/// `compliance_core::insert_suppression` (A.6.4). Enrollment writes are
/// campaign-owned, so the campaigns worker executes them.
struct PauseEnrollments;

#[async_trait]
impl JobHandler for PauseEnrollments {
    fn kind(&self) -> &'static str {
        "compliance.pause_enrollments"
    }

    async fn handle(
        &self,
        ctx: &JobCtx,
        tx: &mut Tx<'_>,
        payload: serde_json::Value,
    ) -> Result<(), AppError> {
        let addr = payload_uuid(&payload, "email_address_id")?;
        let suppression_id = payload_uuid(&payload, "suppression_id").ok();
        let actor = system_actor(tx).await?;
        application::campaigns::pause_enrollments(tx, actor, ctx.job_id, addr, suppression_id).await
    }
}

// --------------------------------------------------------------- events

/// Campaign-facing event effects (integration-contracts §"Outbox events"):
/// outbound.accepted → schedule next step; outbound.failed → stop the
/// enrollment; message.received / booking.requested → §8.4 pauses.
struct CampaignEvents;

#[async_trait]
impl EventConsumer for CampaignEvents {
    fn name(&self) -> &'static str {
        "campaigns.flow"
    }

    fn event_types(&self) -> &'static [&'static str] {
        &[
            "outbound.accepted",
            "outbound.failed",
            "message.received",
            "booking.requested",
        ]
    }

    async fn handle(
        &self,
        ctx: &EventCtx,
        tx: &mut Tx<'_>,
        event: &OutboxRow,
    ) -> Result<(), AppError> {
        let actor = system_actor(tx).await?;
        match event.event_type.as_str() {
            "outbound.accepted" => {
                // The payload may carry `campaign_enrollment_id` directly;
                // fall back to the outbound → enrollment lookup for events
                // published by providers that omit it.
                let enrollment_id =
                    match payload_uuid(&event.payload, "campaign_enrollment_id").ok() {
                        Some(id) => Some(id),
                        None => {
                            let ob_id = payload_uuid(&event.payload, "outbound_message_id")?;
                            repo::get_outbound_enrollment(tx, ob_id).await?
                        }
                    };
                let Some(enrollment_id) = enrollment_id else {
                    return Ok(()); // not a campaign message
                };
                application::campaigns::schedule_next_step(tx, actor, ctx.event_id, enrollment_id)
                    .await
            }
            "outbound.failed" => {
                let ob_id = payload_uuid(&event.payload, "outbound_message_id")?;
                application::campaigns::mark_enrollment_failed(tx, actor, Some(ctx.event_id), ob_id)
                    .await
            }
            "message.received" => {
                // The payload's `classification` uses the email_messages DDL
                // values (integration-contracts §events). Human reply classes
                // all map to the `human_reply` pause reason; `bounce` is
                // handled by compliance's suppression path, not here.
                let classification = event
                    .payload
                    .get("classification")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let reason = match classification {
                    "human_positive" | "human_negative" | "question" => {
                        domain::campaigns::pause_reason::HUMAN_REPLY
                    }
                    "auto_reply" => domain::campaigns::pause_reason::AUTO_REPLY_REVIEW,
                    "unsubscribe_request" => domain::campaigns::pause_reason::UNSUBSCRIBE_REQUEST,
                    _ => return Ok(()), // bounce / unknown don't pause here
                };
                let addr = match payload_uuid(&event.payload, "from_email_address_id").ok() {
                    Some(a) => Some(a),
                    None => match payload_uuid(&event.payload, "email_message_id") {
                        Ok(mid) => repo::message_from_address_id(tx, mid).await?,
                        Err(_) => None,
                    },
                };
                let Some(addr) = addr else { return Ok(()) };
                application::campaigns::pause_address(tx, actor, ctx.event_id, addr, reason).await
            }
            "booking.requested" => {
                let Some(addr) = payload_uuid(&event.payload, "email_address_id").ok() else {
                    return Ok(());
                };
                application::campaigns::pause_address(
                    tx,
                    actor,
                    ctx.event_id,
                    addr,
                    domain::campaigns::pause_reason::MEETING_REQUESTED,
                )
                .await
            }
            _ => Ok(()),
        }
    }
}
