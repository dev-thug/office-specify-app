//! `sending.dispatch_outbound` + `sending.reconcile_unknown` orchestration
//! (spec §8.3–§8.5, A.6.4, ADR-006).
//!
//! Transaction layout (the submitting boundary is durable BEFORE any
//! external call):
//!   tx1: outbound FOR UPDATE → §8.4 chain → send_attempt 'started' +
//!        outbound 'submitting' + claim → 'submitted'  (COMMIT)
//!   ——— provider call — happens with no DB tx open ———
//!   tx2: outbound FOR UPDATE → attempt/outbound outcome + outbox +
//!        audit + in-flight-suppression record          (COMMIT)
//!
//! `unknown` is never resubmitted: the reconcile job only calls
//! `lookup_sent`. A `submitting` row whose owning job died is isolated to
//! `unknown` first (crash recovery) — never re-sent blindly (T13).

use std::sync::Arc;

use chrono::Utc;
use contracts::AppError;
use uuid::Uuid;

use infrastructure::audit::{self, AuditEntry};
use infrastructure::db::Db;
use infrastructure::jobs::JobCtx;
use infrastructure::outbox;
use infrastructure::providers::{
    DeliveryCapabilities, DeliveryProvider, MailCapabilities, MailProvider, ProviderError,
    ProviderMessage, SendOutcome, SendRequest, SentLookup,
};
use infrastructure::repo::compliance_core;
use infrastructure::repo::sending::{self, OutboundRow};

use super::policy;

pub const DISPATCH_KIND: &str = "sending.dispatch_outbound";
pub const RECONCILE_KIND: &str = "sending.reconcile_unknown";

/// Outbound lifecycle events (source §16.2 core event list).
pub const EV_OUTBOUND_ACCEPTED: &str = "outbound.accepted";
pub const EV_OUTBOUND_FAILED: &str = "outbound.failed";
pub const EV_OUTBOUND_UNKNOWN: &str = "outbound.unknown";

/// Provider boundary — handlers/tests inject fakes; production wires the
/// configured adapters (never real Zoho/delivery in this wave).
#[derive(Clone)]
pub struct Providers {
    pub mail: Arc<dyn MailProvider>,
    pub delivery: Arc<dyn DeliveryProvider>,
}

struct MailBindingUnavailable;

#[async_trait::async_trait]
impl MailProvider for MailBindingUnavailable {
    fn capabilities(&self) -> MailCapabilities {
        MailCapabilities {
            supports_send: false,
            supports_delta_sync: false,
        }
    }

    async fn list_messages(
        &self,
        _cursor: Option<String>,
        _limit: u32,
    ) -> Result<(Vec<ProviderMessage>, Option<String>), ProviderError> {
        Err(ProviderError::NotConfigured)
    }

    async fn get_message(
        &self,
        _provider_message_id: &str,
    ) -> Result<ProviderMessage, ProviderError> {
        Err(ProviderError::NotConfigured)
    }

    async fn send(&self, _request: SendRequest) -> Result<SendOutcome, ProviderError> {
        Err(ProviderError::NotConfigured)
    }

    async fn lookup_sent(
        &self,
        _correlation_id: &str,
    ) -> Result<Option<SentLookup>, ProviderError> {
        Err(ProviderError::NotConfigured)
    }
}

/// Resolve a mailbox adapter or channel-bound delivery adapter before Phase A
/// records a send attempt. SMTP binding uses only the immutable workspace and
/// delivery-channel IDs; message headers never select a connection.
async fn providers_for_outbound(
    db: &Db,
    job: &JobCtx,
    providers: &Providers,
    outbound_id: Uuid,
) -> Result<Providers, AppError> {
    enum Binding {
        Mailbox(Uuid),
        DeliveryChannel(Uuid),
    }
    let binding = {
        let mut tx = db.begin(job.workspace_id).await?;
        let Some(outbound) = sending::get_outbound(&mut tx, outbound_id).await? else {
            tx.commit().await?;
            return Ok(providers.clone());
        };
        let candidate = if matches!(
            outbound.status.as_str(),
            "draft" | "accepted" | "failed" | "canceled" | "unknown" | "submitting"
        ) {
            None
        } else {
            sending::channel_state(&mut tx, outbound.delivery_channel_id)
                .await?
                .map(|channel| match channel.provider.as_str() {
                    "zoho_mail" => Binding::Mailbox(outbound.mail_account_id),
                    _ => Binding::DeliveryChannel(outbound.delivery_channel_id),
                })
        };
        tx.commit().await?;
        candidate
    };
    let Some(binding) = binding else {
        return Ok(providers.clone());
    };
    let mut bound = providers.clone();
    match binding {
        Binding::Mailbox(mail_account_id) => match providers
            .mail
            .for_mailbox(job.workspace_id, mail_account_id)
            .await
        {
            Ok(Some(mailbox_provider)) => bound.mail = mailbox_provider,
            Ok(None) => {}
            Err(ProviderError::NotConfigured | ProviderError::ReauthRequired) => {
                bound.mail = Arc::new(MailBindingUnavailable);
            }
            Err(ProviderError::RateLimited {
                retry_after_seconds,
            }) => {
                return Err(AppError::new(
                    contracts::ErrorCode::RateLimited,
                    "mailbox provider rate limited before submission",
                )
                .with_retry_after(retry_after_seconds));
            }
            Err(ProviderError::Unavailable(_)) => {
                return Err(AppError::dependency_unavailable(
                    "mailbox provider could not be resolved before submission",
                ));
            }
        },
        Binding::DeliveryChannel(channel_id) => match providers
            .delivery
            .for_channel(job.workspace_id, channel_id)
            .await
        {
            Ok(Some(delivery_provider)) => bound.delivery = delivery_provider,
            Ok(None) => {}
            Err(ProviderError::NotConfigured | ProviderError::ReauthRequired) => {
                bound.delivery = Arc::new(DeliveryBindingUnavailable);
            }
            Err(ProviderError::RateLimited {
                retry_after_seconds,
            }) => {
                return Err(AppError::new(
                    contracts::ErrorCode::RateLimited,
                    "delivery provider rate limited before submission",
                )
                .with_retry_after(retry_after_seconds));
            }
            Err(ProviderError::Unavailable(_)) => {
                return Err(AppError::dependency_unavailable(
                    "delivery provider could not be resolved before submission",
                ));
            }
        },
    }
    Ok(bound)
}

struct DeliveryBindingUnavailable;

#[async_trait::async_trait]
impl DeliveryProvider for DeliveryBindingUnavailable {
    fn capabilities(&self) -> DeliveryCapabilities {
        DeliveryCapabilities::default()
    }

    async fn send(&self, _request: SendRequest) -> Result<SendOutcome, ProviderError> {
        Err(ProviderError::NotConfigured)
    }

    async fn lookup_sent(
        &self,
        _correlation_id: &str,
    ) -> Result<Option<SentLookup>, ProviderError> {
        Ok(None)
    }
}

fn audit_system(
    action: &str,
    target_type: &str,
    target_id: Option<Uuid>,
    requested_by: Option<Uuid>,
    job_id: Uuid,
    metadata: serde_json::Value,
) -> AuditEntry {
    AuditEntry {
        actor_principal_id: None,
        requested_by,
        approved_by: None,
        job_id: Some(job_id),
        action: action.to_string(),
        target_type: target_type.to_string(),
        target_id,
        outcome: "ok".to_string(),
        metadata,
        request_id: format!("job:{job_id}"),
        trace_id: None,
    }
}

/// Outcome of Phase A — what the provider call needs.
#[derive(Debug)]
struct Armed {
    outbound: OutboundRow,
    channel_provider: String,
    attempt_id: Uuid,
    attempt_no: i32,
    in_reply_to: Option<String>,
}

/// `sending.dispatch_outbound` — payload `{outbound_message_id}`.
pub async fn run_dispatch(
    db: &Db,
    providers: &Providers,
    job: &JobCtx,
    outbound_id: Uuid,
) -> Result<(), AppError> {
    let routed_providers = providers_for_outbound(db, job, providers, outbound_id).await?;
    // ---------------------------------------------------------------
    // tx1 — authorize + cross the durable submitting boundary.
    // ---------------------------------------------------------------
    let armed: Option<Armed> = {
        let mut tx = db.begin(job.workspace_id).await?;
        let res = dispatch_phase_a(&mut tx, job, &routed_providers, outbound_id).await?;
        match res {
            PhaseA::Done => {
                tx.commit().await?;
                return Ok(());
            }
            PhaseA::Armed(a) => {
                tx.commit().await?;
                Some(*a)
            }
        }
    };
    let Some(armed) = armed else { return Ok(()) };

    // ---------------------------------------------------------------
    // Phase B — external call with no tx open.
    // ---------------------------------------------------------------
    let req = build_request(db, job, &armed).await?;
    let provider_result = match armed.channel_provider.as_str() {
        "zoho_mail" => routed_providers.mail.send(req).await,
        _ => routed_providers.delivery.send(req).await,
    };
    // Once the durable submitting boundary has committed, a transport error
    // can no longer be retried safely. Ambiguous transport failures become
    // `unknown`; explicit pre-send rejection errors stay definitive.
    let outcome = match provider_result {
        Ok(outcome) => outcome,
        Err(infrastructure::providers::ProviderError::Unavailable(_)) => {
            infrastructure::providers::SendOutcome::Unknown {
                reason: "provider transport outcome unavailable after submission".into(),
            }
        }
        Err(infrastructure::providers::ProviderError::NotConfigured) => {
            infrastructure::providers::SendOutcome::Rejected {
                permanent: true,
                code: "PROVIDER_NOT_CONFIGURED".into(),
            }
        }
        Err(infrastructure::providers::ProviderError::ReauthRequired) => {
            infrastructure::providers::SendOutcome::Rejected {
                permanent: true,
                code: "INTEGRATION_REAUTH_REQUIRED".into(),
            }
        }
        Err(infrastructure::providers::ProviderError::RateLimited { .. }) => {
            infrastructure::providers::SendOutcome::Rejected {
                permanent: false,
                code: "PROVIDER_RATE_LIMITED".into(),
            }
        }
    };

    // ---------------------------------------------------------------
    // tx2 — persist the outcome + the in-flight-suppression record.
    // ---------------------------------------------------------------
    let mut tx = db.begin(job.workspace_id).await?;
    dispatch_phase_c(&mut tx, job, &armed, &outcome).await?;
    tx.commit().await?;
    Ok(())
}

enum PhaseA {
    Done,
    Armed(Box<Armed>),
}

async fn dispatch_phase_a(
    tx: &mut infrastructure::db::Tx<'_>,
    job: &JobCtx,
    providers: &Providers,
    outbound_id: Uuid,
) -> Result<PhaseA, AppError> {
    let Some(o) = sending::get_outbound_for_update(tx, outbound_id).await? else {
        return Ok(PhaseA::Done); // vanished/other workspace — nothing to do
    };

    match o.status.as_str() {
        "draft" | "accepted" | "failed" | "canceled" | "unknown" => {
            // Idempotent no-op — someone already moved it.
            return Ok(PhaseA::Done);
        }
        "submitting" => {
            // A previous attempt crossed the boundary. Only isolate when
            // the owning job is provably dead — never double-submit.
            if attempt_owner_alive(tx, &o).await? {
                return Ok(PhaseA::Done);
            }
            isolate_submitting(tx, job, &o).await?;
            return Ok(PhaseA::Done);
        }
        _ => {}
    }

    // Campaigns now contain one immediate message only. A queued legacy
    // follow-up can outlive the campaign workflow worker or arrive from an
    // old retry, so enforce the retirement at the last pre-provider boundary
    // as well. This check runs before policy evaluation/attempt creation and
    // cancels the row plus any queued dispatch job without touching its
    // frozen content.
    if let (Some(revision_id), Some(step_id)) = (o.revision_id, o.step_id) {
        let step_order: Option<i32> = sqlx::query_scalar(
            "SELECT step_order FROM campaign_steps
              WHERE workspace_id = $1 AND revision_id = $2 AND id = $3",
        )
        .bind(tx.workspace_id())
        .bind(revision_id)
        .bind(step_id)
        .fetch_optional(tx.conn())
        .await
        .map_err(infrastructure::error::map_sqlx)?;
        if step_order.is_some_and(|order| order > 1) {
            cancel(tx, job, &o, "CAMPAIGN_FOLLOW_UP_DISABLED").await?;
            return Ok(PhaseA::Done);
        }
    }

    // A.6.4: lock the recipient address BEFORE suppression/basis reads —
    // the same order insert_suppression uses, so a committed suppression
    // is always seen here.
    if !compliance_core::lock_email_address(tx, o.to_email_address_id).await? {
        cancel(tx, job, &o, "SEND_BASIS_UNVERIFIED").await?;
        return Ok(PhaseA::Done);
    }
    if !sending::lock_channel(tx, o.delivery_channel_id).await? {
        cancel(tx, job, &o, policy::CODE_CHANNEL_MISSING).await?;
        return Ok(PhaseA::Done);
    }

    let expected = policy::expected_last_inbound(tx, &o).await?;
    let checks = policy::evaluate(tx, &o, expected).await?;

    if let Some(failed) = checks.iter().find(|c| !c.passed) {
        let code = failed.code.unwrap_or("VALIDATION_FAILED");
        if policy::is_defer(code) {
            let until = failed
                .defer_until
                .unwrap_or_else(|| Utc::now() + policy::POLL_DEFER);
            defer(tx, job, &o, code, until).await?;
        } else {
            cancel(tx, job, &o, code).await?;
        }
        return Ok(PhaseA::Done);
    }

    let Some(chan) = sending::channel_state(tx, o.delivery_channel_id).await? else {
        cancel(tx, job, &o, policy::CODE_CHANNEL_MISSING).await?;
        return Ok(PhaseA::Done);
    };
    if chan.provider == "zoho_campaigns" {
        cancel(tx, job, &o, "ZOHO_CAMPAIGNS_REQUIRES_BULK_EXPORT").await?;
        return Ok(PhaseA::Done);
    }
    let provider_can_send = if chan.provider == "zoho_mail" {
        providers.mail.capabilities().supports_send
    } else {
        providers.delivery.capabilities().supports_send
    };
    if !provider_can_send {
        defer(
            tx,
            job,
            &o,
            "PROVIDER_NOT_CONFIGURED",
            Utc::now() + policy::POLL_DEFER,
        )
        .await?;
        return Ok(PhaseA::Done);
    }

    // T30 restore gate: re-read the shared operations flag in the same
    // permission transaction immediately before recording the submission
    // attempt. A prequeued dispatch stays queued and is rearmed while locked.
    if infrastructure::repo::operations::send_workers_locked(tx).await? {
        defer(
            tx,
            job,
            &o,
            policy::CODE_SEND_WORKERS_LOCKED,
            Utc::now() + policy::POLL_DEFER,
        )
        .await?;
        return Ok(PhaseA::Done);
    }

    // Legacy or directly restored marketing snapshots must still carry an
    // active unsubscribe capability bound to this outbound and recipient.
    // This runs in the persisted permission tx before the send_attempt and
    // submitting boundary, so an invalid/missing token cannot reach provider
    // I/O. The T30 lock is checked first so restore mode preserves queued work.
    if o.purpose == "marketing" {
        let candidates =
            domain::sending::unsubscribe_token_candidates(&o.body_text, o.body_html.as_deref());
        let mut has_bound_capability = false;
        for token in candidates {
            if sending::unsubscribe_token_belongs_to_outbound(
                tx,
                o.to_email_address_id,
                o.id,
                &token,
            )
            .await?
            {
                has_bound_capability = true;
                break;
            }
        }
        if !has_bound_capability {
            cancel(tx, job, &o, "UNSUBSCRIBE_CAPABILITY_MISSING").await?;
            return Ok(PhaseA::Done);
        }
    }

    // All predicates passed — cross the durable submitting boundary.
    let attempt_no = sending::next_attempt_no(tx, o.id).await?;
    let attempt_id = sending::insert_send_attempt(tx, o.id, attempt_no, job.lease_token).await?;
    sending::mark_submitting(tx, o.id).await?;
    sending::transition_claim(tx, o.id, &["reserved"], "submitted").await?;
    audit::record(
        tx,
        audit_system(
            "send.dispatched",
            "outbound_message",
            Some(o.id),
            Some(o.requested_by),
            job.job_id,
            serde_json::json!({"attempt_no": attempt_no, "channel": chan.provider}),
        ),
    )
    .await?;

    let in_reply_to = match o.reply_to_message_id {
        Some(m) => sending::message_internet_id(tx, m).await?,
        None => None,
    };

    Ok(PhaseA::Armed(Box::new(Armed {
        channel_provider: chan.provider.clone(),
        outbound: o,
        attempt_id,
        attempt_no,
        in_reply_to,
    })))
}

/// Is the job that created the open attempt still running under a live
/// lease? Then this row is in-flight — leave it alone.
async fn attempt_owner_alive(
    tx: &mut infrastructure::db::Tx<'_>,
    o: &OutboundRow,
) -> Result<bool, AppError> {
    let row: Option<(String, Option<chrono::DateTime<Utc>>)> = sqlx::query_as(
        "SELECT j.state, j.lease_expires_at
           FROM send_attempts sa
           JOIN jobs j ON j.workspace_id = sa.workspace_id
                      AND j.lease_token = sa.lease_token
          WHERE sa.workspace_id = $1 AND sa.outbound_message_id = $2
          ORDER BY sa.attempt_no DESC LIMIT 1",
    )
    .bind(tx.workspace_id())
    .bind(o.id)
    .fetch_optional(tx.conn())
    .await
    .map_err(infrastructure::error::map_sqlx)?;
    Ok(matches!(
        row,
        Some((ref s, Some(exp))) if s == "running" && exp > Utc::now()
    ))
}

/// §8.5: a `submitting` row with a dead owner is isolated to `unknown`
/// and reconciled — never resubmitted.
async fn isolate_submitting(
    tx: &mut infrastructure::db::Tx<'_>,
    job: &JobCtx,
    o: &OutboundRow,
) -> Result<(), AppError> {
    if let Some(att) = sending::latest_attempt(tx, o.id).await? {
        sending::finish_send_attempt(
            tx,
            att.id,
            "unknown",
            None,
            Some("PROVIDER_SUBMISSION_OUTCOME_UNKNOWN"),
        )
        .await?;
    }
    sending::set_status(
        tx,
        o.id,
        "unknown",
        Some("PROVIDER_SUBMISSION_OUTCOME_UNKNOWN"),
    )
    .await?;
    outbox::emit(
        tx,
        EV_OUTBOUND_UNKNOWN,
        "outbound_message",
        o.id,
        o.version + 1,
        serde_json::json!({"outbound_message_id": o.id, "reason": "submitting_owner_lost"}),
    )
    .await?;
    enqueue_reconcile(tx, o.id, Utc::now() + policy::RECONCILE_DEFER).await?;
    audit::record(
        tx,
        audit_system(
            "send.isolated_submitting",
            "outbound_message",
            Some(o.id),
            Some(o.requested_by),
            job.job_id,
            serde_json::json!({"reason": "owning job lease lost"}),
        ),
    )
    .await?;
    Ok(())
}

/// Terminal policy failure — cancel with the failure code, release the
/// reply claim, and stop any other queued dispatch jobs for the row.
async fn cancel(
    tx: &mut infrastructure::db::Tx<'_>,
    job: &JobCtx,
    o: &OutboundRow,
    code: &str,
) -> Result<(), AppError> {
    sending::set_status(tx, o.id, "canceled", Some(code)).await?;
    sending::transition_claim(tx, o.id, &["reserved", "submitted"], "released").await?;
    sending::cancel_queued_dispatch_jobs(tx, o.id).await?;
    audit::record(
        tx,
        audit_system(
            "send.canceled",
            "outbound_message",
            Some(o.id),
            Some(o.requested_by),
            job.job_id,
            serde_json::json!({"failure_code": code}),
        ),
    )
    .await?;
    Ok(())
}

/// Temporary block — stay `queued`, rearm a dispatch for `until`. Dedupe
/// key carries the minute bucket so repeated defers are cheap and unique.
async fn defer(
    tx: &mut infrastructure::db::Tx<'_>,
    job: &JobCtx,
    o: &OutboundRow,
    code: &str,
    until: chrono::DateTime<Utc>,
) -> Result<(), AppError> {
    let bucket = until.timestamp() / 60;
    sending::enqueue_once(
        tx,
        DISPATCH_KIND,
        1,
        serde_json::json!({"outbound_message_id": o.id}),
        until,
        &format!("dispatch_rearm:{}:{bucket}", o.id),
    )
    .await?;
    audit::record(
        tx,
        audit_system(
            "send.deferred",
            "outbound_message",
            Some(o.id),
            Some(o.requested_by),
            job.job_id,
            serde_json::json!({"code": code, "until": until}),
        ),
    )
    .await?;
    Ok(())
}

async fn enqueue_reconcile(
    tx: &mut infrastructure::db::Tx<'_>,
    outbound_id: Uuid,
    at: chrono::DateTime<Utc>,
) -> Result<(), AppError> {
    let count = sending::reconcile_job_count(tx, outbound_id).await?;
    if count >= policy::MAX_RECONCILE_JOBS {
        return Ok(()); // bounded polling — unknown stays for an operator
    }
    sending::enqueue_once(
        tx,
        RECONCILE_KIND,
        1,
        serde_json::json!({"outbound_message_id": outbound_id}),
        at,
        &format!("reconcile:{outbound_id}:{count}"),
    )
    .await?;
    Ok(())
}

/// §11.3 provider request construction — unsubscribe headers for
/// marketing, In-Reply-To for replies.
async fn build_request(db: &Db, _job: &JobCtx, armed: &Armed) -> Result<SendRequest, AppError> {
    let o = &armed.outbound;
    let mut headers: Vec<(String, String)> = Vec::new();
    headers.push(("X-Outreach-Correlation".to_string(), o.id.to_string()));
    if let Some(url) = one_click_unsubscribe_url(db, o).await? {
        headers.push(("List-Unsubscribe".to_string(), format!("<{url}>")));
        headers.push((
            "List-Unsubscribe-Post".to_string(),
            "List-Unsubscribe=One-Click".to_string(),
        ));
    }
    Ok(SendRequest {
        correlation_id: o.id.to_string(),
        from: o.from_address_snapshot.clone(),
        to: o.to_address_snapshot.clone(),
        subject: o.subject.clone(),
        body_text: Some(o.body_text.clone()),
        body_html: o.body_html.clone(),
        in_reply_to: armed.in_reply_to.clone(),
        headers,
    })
}

/// Derive a standards-compliant one-click endpoint from the immutable body
/// link. Both current confirmation-page links and older frozen API URLs are
/// recognized. The candidate token must hash to an active capability bound
/// to this exact outbound/recipient; the host is always rebuilt from the
/// configured public origin, never copied from message content.
async fn one_click_unsubscribe_url(
    db: &Db,
    outbound: &OutboundRow,
) -> Result<Option<String>, AppError> {
    if outbound.purpose != "marketing" {
        return Ok(None);
    }
    let candidates = domain::sending::unsubscribe_token_candidates(
        &outbound.body_text,
        outbound.body_html.as_deref(),
    );
    if candidates.is_empty() {
        return Ok(None);
    }

    let mut tx = db.begin(outbound.workspace_id).await?;
    // Only SMTP currently has an enabled Office one-click handler. Do not
    // advertise a POST action for other adapters whose endpoint is gated.
    let smtp_bound: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM smtp_connections
          WHERE workspace_id = $1 AND channel_id = $2)",
    )
    .bind(outbound.workspace_id)
    .bind(outbound.delivery_channel_id)
    .fetch_one(tx.conn())
    .await
    .map_err(infrastructure::error::map_sqlx)?;
    if !smtp_bound {
        tx.rollback().await?;
        return Ok(None);
    }
    for token in candidates {
        if sending::unsubscribe_token_belongs_to_outbound(
            &mut tx,
            outbound.to_email_address_id,
            outbound.id,
            &token,
        )
        .await?
        {
            let origin = sending::public_base_url(&mut tx).await?;
            tx.commit().await?;
            // RFC 8058 requires HTTPS. Local development still uses the
            // normal Office confirmation link in the message body.
            if !origin.starts_with("https://") {
                return Ok(None);
            }
            return Ok(Some(format!(
                "{}/public/v1/unsubscribe/{token}/one-click",
                origin.trim_end_matches('/')
            )));
        }
    }
    tx.rollback().await?;
    Ok(None)
}

/// tx2 — record the provider outcome. `unknown` emits the event + enqueues
/// reconcile; accepted/failed are terminal. An active suppression observed
/// here means the send was in-flight at suppression commit — audited as
/// `send.in_flight_at_suppression` (§8.6/T12).
async fn dispatch_phase_c(
    tx: &mut infrastructure::db::Tx<'_>,
    job: &JobCtx,
    armed: &Armed,
    outcome: &infrastructure::providers::SendOutcome,
) -> Result<(), AppError> {
    use infrastructure::providers::SendOutcome;
    let o = &armed.outbound;
    let Some(current) = sending::get_outbound_for_update(tx, o.id).await? else {
        return Ok(());
    };
    if current.status != "submitting" {
        // Someone else resolved it (cancel/isolation) — do not overwrite.
        return Ok(());
    }

    match outcome {
        SendOutcome::Accepted {
            provider_message_id,
        } => {
            sending::finish_send_attempt(
                tx,
                armed.attempt_id,
                "accepted",
                Some(provider_message_id),
                None,
            )
            .await?;
            sending::mark_accepted(tx, o.id, provider_message_id, Utc::now()).await?;
            sending::transition_claim(tx, o.id, &["submitted", "reserved"], "accepted").await?;
            // Provider-confirmed acceptance — never "delivered" (T34).
            sending::insert_delivery_event(
                tx,
                o.id,
                &format!("{}:{provider_message_id}:accepted", armed.channel_provider),
                "accepted",
                Utc::now(),
                &serde_json::json!({}),
            )
            .await?;
            outbox::emit(
                tx,
                EV_OUTBOUND_ACCEPTED,
                "outbound_message",
                o.id,
                current.version + 1,
                serde_json::json!({
                    "outbound_message_id": o.id,
                    "campaign_enrollment_id": o.enrollment_id,
                    "email_address_id": o.to_email_address_id,
                    "thread_id": o.thread_id,
                }),
            )
            .await?;
            audit::record(
                tx,
                audit_system(
                    "send.accepted",
                    "outbound_message",
                    Some(o.id),
                    Some(o.requested_by),
                    job.job_id,
                    serde_json::json!({"attempt_no": armed.attempt_no}),
                ),
            )
            .await?;
        }
        SendOutcome::Rejected { permanent, code } => {
            sending::finish_send_attempt(
                tx,
                armed.attempt_id,
                "definitive_failed",
                None,
                Some(code),
            )
            .await?;
            sending::set_status(tx, o.id, "failed", Some(code)).await?;
            sending::transition_claim(tx, o.id, &["submitted", "reserved"], "released").await?;
            outbox::emit(
                tx,
                EV_OUTBOUND_FAILED,
                "outbound_message",
                o.id,
                current.version + 1,
                serde_json::json!({
                    "outbound_message_id": o.id,
                    "permanent": permanent,
                    "code": code,
                }),
            )
            .await?;
            audit::record(
                tx,
                audit_system(
                    "send.failed",
                    "outbound_message",
                    Some(o.id),
                    Some(o.requested_by),
                    job.job_id,
                    serde_json::json!({"code": code, "permanent": permanent}),
                ),
            )
            .await?;
        }
        SendOutcome::Unknown { reason } => {
            sending::finish_send_attempt(
                tx,
                armed.attempt_id,
                "unknown",
                None,
                Some("PROVIDER_SUBMISSION_OUTCOME_UNKNOWN"),
            )
            .await?;
            sending::set_status(
                tx,
                o.id,
                "unknown",
                Some("PROVIDER_SUBMISSION_OUTCOME_UNKNOWN"),
            )
            .await?;
            outbox::emit(
                tx,
                EV_OUTBOUND_UNKNOWN,
                "outbound_message",
                o.id,
                current.version + 1,
                serde_json::json!({
                    "outbound_message_id": o.id,
                    "reason": reason,
                }),
            )
            .await?;
            enqueue_reconcile(tx, o.id, Utc::now() + policy::RECONCILE_DEFER).await?;
            audit::record(
                tx,
                audit_system(
                    "send.unknown",
                    "outbound_message",
                    Some(o.id),
                    Some(o.requested_by),
                    job.job_id,
                    serde_json::json!({"reason": reason}),
                ),
            )
            .await?;
        }
    }

    // §8.6: suppression committed while this send was in flight is recorded
    // (the send itself is beyond recall — later sends will be blocked).
    if compliance_core::lock_email_address(tx, o.to_email_address_id).await? {
        let sup =
            compliance_core::active_suppressions(tx, o.to_email_address_id, &o.purpose).await?;
        if !sup.is_empty() {
            audit::record(
                tx,
                audit_system(
                    "send.in_flight_at_suppression",
                    "outbound_message",
                    Some(o.id),
                    Some(o.requested_by),
                    job.job_id,
                    serde_json::json!({
                        "suppression_ids": sup.iter().map(|s| s.id).collect::<Vec<_>>(),
                    }),
                ),
            )
            .await?;
        }
    }
    Ok(())
}

/// `sending.reconcile_unknown` — LOOKUP ONLY. Never resubmits (ADR-006).
pub async fn run_reconcile(
    db: &Db,
    providers: &Providers,
    job: &JobCtx,
    outbound_id: Uuid,
) -> Result<(), AppError> {
    // Phase A — load + isolate dead submissions.
    enum Next {
        Done,
        Lookup {
            outbound: Box<OutboundRow>,
            provider: String,
            channel_id: Uuid,
        },
    }
    let next = {
        let mut tx = db.begin(job.workspace_id).await?;
        let Some(o) = sending::get_outbound_for_update(&mut tx, outbound_id).await? else {
            tx.commit().await?;
            return Ok(());
        };
        let n = match o.status.as_str() {
            "submitting" => {
                if attempt_owner_alive(&mut tx, &o).await? {
                    Next::Done
                } else {
                    isolate_submitting(&mut tx, job, &o).await?;
                    Next::Done
                }
            }
            "unknown" => {
                let chan = sending::channel_state(&mut tx, o.delivery_channel_id).await?;
                match chan {
                    Some(c) => Next::Lookup {
                        channel_id: o.delivery_channel_id,
                        outbound: Box::new(o),
                        provider: c.provider,
                    },
                    None => Next::Done,
                }
            }
            _ => Next::Done,
        };
        tx.commit().await?;
        n
    };
    let Next::Lookup {
        outbound: o,
        provider,
        channel_id,
    } = next
    else {
        return Ok(());
    };

    // Phase B — provider read (no tx open).
    let looked: Result<Option<SentLookup>, infrastructure::providers::ProviderError> =
        match provider.as_str() {
            "zoho_mail" => match providers
                .mail
                .for_mailbox(job.workspace_id, o.mail_account_id)
                .await
            {
                Ok(Some(mailbox_provider)) => mailbox_provider.lookup_sent(&o.id.to_string()).await,
                Ok(None) => providers.mail.lookup_sent(&o.id.to_string()).await,
                Err(ProviderError::NotConfigured | ProviderError::ReauthRequired) => Ok(None),
                Err(error) => Err(error),
            },
            _ => match providers
                .delivery
                .for_channel(job.workspace_id, channel_id)
                .await
            {
                Ok(Some(channel_provider)) => channel_provider.lookup_sent(&o.id.to_string()).await,
                Ok(None) => providers.delivery.lookup_sent(&o.id.to_string()).await,
                Err(ProviderError::NotConfigured | ProviderError::ReauthRequired) => Ok(None),
                Err(error) => Err(error),
            },
        };

    // Phase C — commit evidence (or rearm bounded polling).
    let mut tx = db.begin(job.workspace_id).await?;
    let Some(current) = sending::get_outbound_for_update(&mut tx, o.id).await? else {
        tx.commit().await?;
        return Ok(());
    };
    match looked {
        Ok(Some(lookup)) => {
            if current.status != "unknown" {
                tx.commit().await?;
                return Ok(());
            }
            if let Some(att) = sending::latest_attempt(&mut tx, o.id).await? {
                sending::finish_send_attempt(
                    &mut tx,
                    att.id,
                    "accepted",
                    Some(&lookup.provider_message_id),
                    None,
                )
                .await?;
            }
            sending::mark_accepted(&mut tx, o.id, &lookup.provider_message_id, lookup.sent_at)
                .await?;
            sending::transition_claim(&mut tx, o.id, &["submitted", "reserved"], "accepted")
                .await?;
            sending::insert_delivery_event(
                &mut tx,
                o.id,
                &format!("{provider}:{}:accepted", lookup.provider_message_id),
                "accepted",
                lookup.sent_at,
                &serde_json::json!({"reconciled": true}),
            )
            .await?;
            outbox::emit(
                &mut tx,
                EV_OUTBOUND_ACCEPTED,
                "outbound_message",
                o.id,
                current.version + 1,
                serde_json::json!({
                    "outbound_message_id": o.id,
                    "campaign_enrollment_id": o.enrollment_id,
                    "email_address_id": o.to_email_address_id,
                    "thread_id": o.thread_id,
                    "reconciled": true,
                }),
            )
            .await?;
            audit::record(
                &mut tx,
                audit_system(
                    "send.reconciled_accepted",
                    "outbound_message",
                    Some(o.id),
                    Some(o.requested_by),
                    job.job_id,
                    serde_json::json!({}),
                ),
            )
            .await?;
        }
        Ok(None) => {
            // Absence of evidence is not evidence of absence — stay unknown,
            // poll bounded, never resubmit.
            enqueue_reconcile(&mut tx, o.id, Utc::now() + policy::RECONCILE_DEFER).await?;
            audit::record(
                &mut tx,
                audit_system(
                    "send.reconcile_no_evidence",
                    "outbound_message",
                    Some(o.id),
                    Some(o.requested_by),
                    job.job_id,
                    serde_json::json!({}),
                ),
            )
            .await?;
        }
        Err(e) => {
            // Provider read failed — the job-level retry is safe (read only).
            let _ = tx.rollback().await;
            return Err(match e {
                infrastructure::providers::ProviderError::Unavailable(m) => {
                    AppError::dependency_unavailable(m)
                }
                infrastructure::providers::ProviderError::NotConfigured => AppError::new(
                    contracts::ErrorCode::IntegrationReauthRequired,
                    "provider is not configured for reconciliation",
                ),
                infrastructure::providers::ProviderError::ReauthRequired => AppError::new(
                    contracts::ErrorCode::IntegrationReauthRequired,
                    "provider reauth required for reconciliation",
                ),
                infrastructure::providers::ProviderError::RateLimited {
                    retry_after_seconds,
                } => AppError::new(contracts::ErrorCode::RateLimited, "provider rate limited")
                    .with_retry_after(retry_after_seconds),
            });
        }
    }
    tx.commit().await?;
    Ok(())
}
