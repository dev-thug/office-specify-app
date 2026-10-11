//! Send-flow use cases, all running inside worker transactions:
//! `campaigns.activate_launch`, `campaigns.schedule_next_step`,
//! `compliance.pause_enrollments`, `campaigns.pause_address`, and the
//! `outbound.failed` enrollment update. Each only mutates execution-state
//! columns the A.6.2 freeze guards permit.

use chrono::{DateTime, Utc};
use contracts::AppError;
use domain::campaigns as d;
use infrastructure::audit;
use infrastructure::db::Tx;
use infrastructure::repo::approvals as approvals_repo;
use infrastructure::repo::campaigns as repo;
use infrastructure::repo::compliance_core;
use uuid::Uuid;

use super::campaign::audited_system;

/// Cancel queued `sending.dispatch_outbound` jobs for the enrollments'
/// not-yet-submitted rows, then cancel the rows themselves.
async fn cancel_pending_sends(tx: &mut Tx<'_>, enrollment_ids: &[Uuid]) -> Result<(), AppError> {
    let outbound_ids = repo::pending_outbound_ids_for_enrollments(tx, enrollment_ids).await?;
    repo::cancel_pending_outbounds_for_enrollments(tx, enrollment_ids).await?;
    let keys: Vec<String> = outbound_ids
        .iter()
        .map(|id| repo::dispatch_dedupe_key(*id))
        .collect();
    repo::cancel_queued_jobs(tx, "sending.dispatch_outbound", &keys).await?;
    Ok(())
}

/// Queue the frozen outbound for one activated enrollment. Only the
/// activation path supplies explicit_submission_at; legacy scheduling keeps
/// the dispatch gate closed.
async fn queue_step_dispatch(
    tx: &mut Tx<'_>,
    enrollment_id: Uuid,
    step_id: Uuid,
    send_at: DateTime<Utc>,
    explicit_submission_at: Option<DateTime<Utc>>,
) -> Result<(), AppError> {
    if let Some(ob) = repo::outbound_for_enrollment_step(tx, enrollment_id, step_id).await? {
        let queued = match explicit_submission_at.as_ref() {
            Some(at) => repo::queue_outbound_at_explicit(tx, ob.id, send_at, *at).await?,
            None => repo::queue_outbound_at(tx, ob.id, send_at).await?,
        };
        if queued {
            let mut payload = serde_json::json!({"outbound_message_id": ob.id});
            if let Some(at) = explicit_submission_at.as_ref() {
                payload["explicit_submission_at"] = serde_json::json!(at);
            }
            repo::enqueue_dedup(
                tx,
                "sending.dispatch_outbound",
                payload,
                send_at,
                &repo::dispatch_dedupe_key(ob.id),
            )
            .await?;
        }
    }
    Ok(())
}

async fn validated_launch_submission_at(
    tx: &mut Tx<'_>,
    launch: &repo::LaunchRow,
    revision: &repo::RevisionRow,
) -> Result<Option<DateTime<Utc>>, AppError> {
    let Some(job) = repo::launch_activation_job(tx, launch.id).await? else {
        return Ok(None);
    };
    if job.dedupe_key != format!("explicit_activate_launch:{}", launch.id) {
        return Ok(None);
    }
    let (Some(approval_request_id), Some(explicit_submission_at)) = (
        job.approval_request_id
            .as_deref()
            .and_then(|id| Uuid::parse_str(id).ok()),
        job.explicit_submission_at,
    ) else {
        return Ok(None);
    };
    let campaign = repo::get_campaign(tx, revision.campaign_id)
        .await?
        .ok_or_else(|| AppError::internal("launch campaign missing"))?;
    if !super::launch::validate_activation_executor(tx, &campaign, &job).await? {
        return Ok(None);
    }
    super::launch::validate_approved_launch(tx, launch, Some(approval_request_id)).await?;
    Ok(Some(explicit_submission_at))
}

/// `campaigns.activate_launch` — fires after an explicit approved-launch
/// execution request. Re-verifies executor, approval, requester credentials,
/// and grants before activation, then re-checks suppression/basis per
/// recipient and enqueues the first-step dispatch jobs.
pub async fn activate_launch(
    tx: &mut Tx<'_>,
    actor: Uuid,
    job_id: Uuid,
    launch_id: Uuid,
    approval_request_id: Uuid,
) -> Result<(), AppError> {
    let Some(launch) = repo::get_launch_for_update(tx, launch_id).await? else {
        return Err(AppError::not_found("campaign launch not found"));
    };
    // Approval alone is not an execution authorization. Old jobs created by
    // the prior approval path have no explicit executor and finish as no-ops.
    if launch.status != d::launch_status::APPROVED {
        return Ok(());
    }
    let Some(activation_job) = repo::launch_activation_job(tx, launch_id).await? else {
        return Ok(());
    };
    if activation_job.id != job_id
        || activation_job.dedupe_key != format!("explicit_activate_launch:{launch_id}")
        || activation_job
            .approval_request_id
            .as_deref()
            .and_then(|value| Uuid::parse_str(value).ok())
            != Some(approval_request_id)
    {
        return Ok(());
    }

    let Some(revision) = repo::get_revision(tx, launch.revision_id).await? else {
        return Err(AppError::internal("launch revision missing"));
    };
    let campaign = repo::get_campaign(tx, revision.campaign_id)
        .await?
        .ok_or_else(|| AppError::internal("launch campaign missing"))?;
    if !super::launch::validate_activation_executor(tx, &campaign, &activation_job).await? {
        return Ok(());
    }

    // Re-verify the approval is still live + pins this manifest.
    let Some(req) = approvals_repo::get_request_for_update(tx, approval_request_id).await? else {
        return Err(AppError::internal("approval request missing"));
    };
    if req.status != "approved" {
        // Revoked after approval — the launch must not arm (A.6.3).
        repo::transition_launch(tx, launch_id, d::launch_status::CANCELED, None).await?;
        audit::record(
            tx,
            audited_system(
                actor,
                Some(job_id),
                "campaign.activation_aborted",
                "campaign_launch",
                launch_id,
                serde_json::json!({"reason": "approval_revoked"}),
            ),
        )
        .await?;
        return Ok(());
    }
    if req.expires_at <= Utc::now() || launch.expires_at <= Utc::now() {
        repo::transition_launch(tx, launch_id, d::launch_status::CANCELED, None).await?;
        return Ok(());
    }
    super::launch::validate_approved_launch(tx, &launch, Some(approval_request_id)).await?;

    let channel = repo::get_channel(tx, launch.delivery_channel_id).await?;
    if !channel.as_ref().is_some_and(|channel| {
        channel.provider == "delivery_provider" && channel.purpose == "marketing"
    }) {
        repo::transition_launch(
            tx,
            launch_id,
            d::launch_status::CANCELED,
            Some(launch.version),
        )
        .await?;
        let enrollment_ids =
            repo::stop_launch_enrollments(tx, launch_id, "unsupported_channel").await?;
        cancel_pending_sends(tx, &enrollment_ids).await?;
        repo::sync_campaign_status(tx, revision.campaign_id).await?;
        return Ok(());
    }

    let rules = d::ScheduleRules::from_json(&revision.schedule_rules)
        .map_err(|e| AppError::internal(format!("stored schedule_rules invalid: {e}")))?;
    let tz = d::parse_timezone(&revision.timezone)
        .ok_or_else(|| AppError::internal("stored timezone invalid"))?;
    let steps = repo::list_steps(tx, revision.id).await?;
    let Some(step1) = steps.first() else {
        return Err(AppError::internal("revision has no steps"));
    };

    // Arm the channel? Global policy is re-verified by the send-time chain
    // in the sending module (§8.4); activation only schedules.
    let explicit_submission_at = activation_job.explicit_submission_at.ok_or_else(|| {
        AppError::conflict("explicit activation job is missing its submission timestamp")
    })?;
    let enrollments = repo::list_enrollments(tx, launch_id).await?;
    let mut activated = 0u64;
    for (i, e) in enrollments
        .iter()
        .filter(|e| e.status == d::enrollment_status::PENDING)
        .enumerate()
    {
        let i = i as u64;
        // §8.4 send-time chain re-checks: suppression and basis are
        // re-verified at activation; dispatch re-checks again at send time.
        compliance_core::lock_email_address(tx, e.email_address_id).await?;
        if !compliance_core::active_suppressions(tx, e.email_address_id, "marketing")
            .await?
            .is_empty()
        {
            repo::set_enrollment_state(
                tx,
                e.id,
                d::enrollment_status::SUPPRESSED,
                None,
                None,
                Some("suppressed"),
            )
            .await?;
            continue;
        }
        let Some(send_at) = d::staggered_send_time(Utc::now(), i, &rules, tz) else {
            repo::set_enrollment_state(
                tx,
                e.id,
                d::enrollment_status::STOPPED,
                None,
                None,
                Some("schedule_exhausted"),
            )
            .await?;
            continue;
        };
        if send_at > launch.expires_at {
            repo::set_enrollment_state(
                tx,
                e.id,
                d::enrollment_status::STOPPED,
                None,
                None,
                Some("launch_expired"),
            )
            .await?;
            continue;
        }
        if repo::activate_enrollment(tx, e.id, step1.id, send_at).await? {
            queue_step_dispatch(tx, e.id, step1.id, send_at, Some(explicit_submission_at)).await?;
            activated += 1;
        } else {
            // RECIPIENT_ACTIVE_ELSEWHERE — the partial unique index fired.
            repo::set_enrollment_state(
                tx,
                e.id,
                d::enrollment_status::STOPPED,
                None,
                None,
                Some("active_elsewhere"),
            )
            .await?;
        }
    }

    // approved → running; when nothing can send the launch completes.
    let next = if activated == 0
        && !enrollments
            .iter()
            .any(|e| e.status == d::enrollment_status::ACTIVE)
    {
        d::launch_status::COMPLETED
    } else {
        d::launch_status::RUNNING
    };
    repo::transition_launch(tx, launch_id, next, Some(launch.version)).await?;
    repo::sync_campaign_status(tx, revision.campaign_id).await?;
    audit::record(
        tx,
        audited_system(
            actor,
            Some(job_id),
            "campaign.launch_activated",
            "campaign_launch",
            launch_id,
            serde_json::json!({
                "campaign_id": revision.campaign_id,
                "approval_request_id": approval_request_id,
                "activated": activated,
                "result": next,
            }),
        ),
    )
    .await?;
    Ok(())
}

/// `campaigns.schedule_next_step` — after `outbound.accepted`. Campaign
/// authoring no longer supports follow-up automation. Legacy enrollments
/// finish their accepted message, cancel any queued follow-ups, and retain
/// their original revision rows for audit/history.
pub async fn schedule_next_step(
    tx: &mut Tx<'_>,
    actor: Uuid,
    job_id: Uuid,
    enrollment_id: Uuid,
) -> Result<(), AppError> {
    let Some(e) = repo::get_enrollment_for_update(tx, enrollment_id).await? else {
        return Ok(()); // enrollment deleted/unknown — nothing to schedule.
    };
    if e.status != d::enrollment_status::ACTIVE {
        return Ok(());
    }
    let Some(launch) = repo::get_launch(tx, e.launch_id).await? else {
        return Ok(());
    };
    if launch.status != d::launch_status::RUNNING {
        // Paused/canceled launch — quiesce already stopped its enrollments;
        // a re-delivered event just finds a stopped enrollment.
        return Ok(());
    }
    let steps = repo::list_steps(tx, e.revision_id).await?;
    let current_order = steps
        .iter()
        .find(|s| Some(s.id) == e.current_step_id)
        .map(|s| s.step_order)
        .unwrap_or(0);
    let next_step = steps.iter().find(|s| s.step_order == current_order + 1);

    if let Some(next) = next_step {
        let reason = "follow_up_automation_disabled";
        repo::set_enrollment_state(
            tx,
            enrollment_id,
            d::enrollment_status::STOPPED,
            e.current_step_id,
            None,
            Some(reason),
        )
        .await?;
        cancel_pending_sends(tx, &[enrollment_id]).await?;

        let counts = repo::enrollment_counts(tx, e.launch_id).await?;
        if counts.pending == 0 && counts.active == 0 && launch.status == d::launch_status::RUNNING {
            repo::transition_launch(tx, e.launch_id, d::launch_status::COMPLETED, None).await?;
            if let Some(revision) = repo::get_revision(tx, e.revision_id).await? {
                repo::sync_campaign_status(tx, revision.campaign_id).await?;
            }
        }
        audit::record(
            tx,
            audited_system(
                actor,
                Some(job_id),
                "campaign.enrollment_stopped",
                "campaign_enrollment",
                enrollment_id,
                serde_json::json!({
                    "launch_id": e.launch_id,
                    "reason": reason,
                    "follow_up_step_order": next.step_order,
                }),
            ),
        )
        .await?;
        return Ok(());
    }

    let Some(next) = next_step else {
        // Sequence finished.
        repo::set_enrollment_state(
            tx,
            enrollment_id,
            d::enrollment_status::COMPLETED,
            e.current_step_id,
            None,
            e.stop_reason.as_deref(),
        )
        .await?;
        // Roll the launch up to completed when nothing remains in flight.
        let counts = repo::enrollment_counts(tx, e.launch_id).await?;
        if counts.pending == 0 && counts.active == 0 {
            repo::transition_launch(tx, e.launch_id, d::launch_status::COMPLETED, None).await?;
            let revision = repo::get_revision(tx, e.revision_id).await?;
            if let Some(r) = revision {
                repo::sync_campaign_status(tx, r.campaign_id).await?;
            }
        }
        audit::record(
            tx,
            audited_system(
                actor,
                Some(job_id),
                "campaign.enrollment_completed",
                "campaign_enrollment",
                enrollment_id,
                serde_json::json!({"launch_id": e.launch_id}),
            ),
        )
        .await?;
        return Ok(());
    };

    // Delay anchor: the previous step's accepted outbound's accepted_at.
    let mut anchor = Utc::now();
    if let Some(prev_step_id) = e.current_step_id
        && let Some(ob) =
            repo::outbound_for_enrollment_step(tx, enrollment_id, prev_step_id).await?
        && let Some(t) = repo::outbound_accepted_at(tx, ob.id).await?
    {
        anchor = t;
    }
    let earliest = anchor + chrono::Duration::hours(next.delay_hours as i64);
    let revision = repo::get_revision(tx, e.revision_id)
        .await?
        .ok_or_else(|| AppError::internal("enrollment revision missing"))?;
    let explicit_submission_at = validated_launch_submission_at(tx, &launch, &revision).await?;
    let rules = d::ScheduleRules::from_json(&revision.schedule_rules)
        .map_err(|e| AppError::internal(format!("stored schedule_rules invalid: {e}")))?;
    let tz = d::parse_timezone(&revision.timezone)
        .ok_or_else(|| AppError::internal("stored timezone invalid"))?;
    match d::next_send_time(earliest, &rules, tz) {
        Some(t) if t <= launch.expires_at => {
            repo::set_enrollment_state(
                tx,
                enrollment_id,
                d::enrollment_status::ACTIVE,
                Some(next.id),
                Some(t),
                None,
            )
            .await?;
            queue_step_dispatch(tx, enrollment_id, next.id, t, explicit_submission_at).await?;
            audit::record(
                tx,
                audited_system(
                    actor,
                    Some(job_id),
                    "campaign.step_scheduled",
                    "campaign_enrollment",
                    enrollment_id,
                    serde_json::json!({
                        "launch_id": e.launch_id,
                        "step_order": next.step_order,
                        "next_send_at": t,
                    }),
                ),
            )
            .await?;
        }
        _ => {
            let reason = if launch.expires_at <= Utc::now() {
                "launch_expired"
            } else {
                "schedule_exhausted"
            };
            repo::set_enrollment_state(
                tx,
                enrollment_id,
                d::enrollment_status::STOPPED,
                e.current_step_id,
                None,
                Some(reason),
            )
            .await?;
        }
    }
    Ok(())
}

/// `compliance.pause_enrollments` — suppression committed (A.6.4); pause
/// every non-terminal enrollment of the address. `suppressed` rows are
/// already stopped but still get their pending sends canceled.
pub async fn pause_enrollments(
    tx: &mut Tx<'_>,
    actor: Uuid,
    job_id: Uuid,
    email_address_id: Uuid,
    suppression_id: Option<Uuid>,
) -> Result<(), AppError> {
    compliance_core::lock_email_address(tx, email_address_id).await?;
    let ids = repo::stop_enrollments_for_address(
        tx,
        email_address_id,
        None, // unconditional — suppression is never flag-governed
        d::enrollment_status::SUPPRESSED,
        "suppressed",
        &["pending", "active"],
    )
    .await?;
    cancel_pending_sends(tx, &ids).await?;
    if !ids.is_empty() {
        audit::record(
            tx,
            audited_system(
                actor,
                Some(job_id),
                "campaign.enrollments_suppressed",
                "email_address",
                email_address_id,
                serde_json::json!({
                    "suppression_id": suppression_id,
                    "enrollments": ids.len(),
                }),
            ),
        )
        .await?;
    }
    Ok(())
}

/// `campaigns.pause_address` — §8.4 stop conditions mapped from inbound
/// classification. `reason` ∈ human_reply | auto_reply_review |
/// meeting_requested | unsubscribe_request. Human-reply and booking stops
/// honor the revision's stop_on_reply / stop_on_booking flags; auto-reply
/// review and unsubscribe stop unconditionally.
pub async fn pause_address(
    tx: &mut Tx<'_>,
    actor: Uuid,
    job_id: Uuid,
    email_address_id: Uuid,
    reason: &str,
) -> Result<(), AppError> {
    let flag = d::pause_reason::governed_flag(reason);
    let ids = repo::stop_enrollments_for_address(
        tx,
        email_address_id,
        flag,
        d::enrollment_status::STOPPED,
        reason,
        &["pending", "active"],
    )
    .await?;
    cancel_pending_sends(tx, &ids).await?;
    if !ids.is_empty() {
        audit::record(
            tx,
            audited_system(
                actor,
                Some(job_id),
                "campaign.address_paused",
                "email_address",
                email_address_id,
                serde_json::json!({
                    "reason": reason,
                    "enrollments": ids.len(),
                }),
            ),
        )
        .await?;
    }
    Ok(())
}

/// `outbound.failed` consumer effect — a failed step-1 send stops the
/// enrollment (`send_failed`). Mid-sequence failures also stop: the frozen
/// sequence cannot skip a step (A.6.2 manifest integrity).
pub async fn mark_enrollment_failed(
    tx: &mut Tx<'_>,
    actor: Uuid,
    job_id: Option<Uuid>,
    outbound_message_id: Uuid,
) -> Result<(), AppError> {
    let Some(enrollment_id) = repo::get_outbound_enrollment(tx, outbound_message_id).await? else {
        return Ok(()); // not a campaign outbound
    };
    let Some(e) = repo::get_enrollment_for_update(tx, enrollment_id).await? else {
        return Ok(());
    };
    if e.status != d::enrollment_status::ACTIVE {
        return Ok(());
    }
    repo::set_enrollment_state(
        tx,
        enrollment_id,
        d::enrollment_status::STOPPED,
        e.current_step_id,
        None,
        Some("send_failed"),
    )
    .await?;
    cancel_pending_sends(tx, &[enrollment_id]).await?;
    audit::record(
        tx,
        audited_system(
            actor,
            job_id,
            "campaign.enrollment_stopped",
            "campaign_enrollment",
            enrollment_id,
            serde_json::json!({
                "launch_id": e.launch_id,
                "reason": "send_failed",
                "outbound_message_id": outbound_message_id,
            }),
        ),
    )
    .await?;
    // Check whether the launch drained to completion.
    let counts = repo::enrollment_counts(tx, e.launch_id).await?;
    if counts.pending == 0
        && counts.active == 0
        && let Some(launch) = repo::get_launch(tx, e.launch_id).await?
        && launch.status == d::launch_status::RUNNING
    {
        repo::transition_launch(tx, e.launch_id, d::launch_status::COMPLETED, None).await?;
    }
    Ok(())
}
