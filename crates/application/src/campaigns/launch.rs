//! Launch use cases (B.3.8): create (202 building + freeze job), detail,
//! approval-request creation, pause, cancel. All transitions are
//! forward-only and only touch columns the A.6.2 freeze guards allow.

use chrono::{Duration, Utc};
use contracts::campaigns::{
    CreateLaunchRequest, CreateLaunchResponse, EnrollmentSummary, LaunchApprovalRequestResponse,
    LaunchDetail, LaunchMessageItem, LaunchMessagePage, LaunchSummary,
};
use contracts::{AppError, Ctx, ErrorCode};
use domain::campaigns as d;
use infrastructure::audit;
use infrastructure::db::Tx;
use infrastructure::outbox;
use infrastructure::repo::approvals as approvals_repo;
use infrastructure::repo::campaigns as repo;
use uuid::Uuid;

use super::campaign::{
    audited, decode_hex, decode_page_cursor, encode_page_cursor, hex_of, require_compose_grant,
    version_check,
};
use crate::authz::{self, MailboxPerm};

/// Default approval window when schedule_rules carries no explicit expiry.
const DEFAULT_LAUNCH_TTL_HOURS: i64 = 24 * 7;
/// Safety bound on the explicit recipient list (B.3.8 allows explicit lists;
/// large sends go through segments in a later wave).
const MAX_RECIPIENTS: usize = 5_000;

fn requester_context_snapshot(ctx: &Ctx) -> serde_json::Value {
    let credential_id = match ctx.auth {
        contracts::AuthMethod::ServiceCredential { credential_id } => Some(credential_id),
        _ => None,
    };
    serde_json::json!({
        "principal_id": ctx.principal_id,
        "permission_version": ctx.permission_version,
        "request_id": ctx.request_id,
        "agent_run_id": ctx.agent_run_id,
        "credential_id": credential_id,
    })
}

fn launch_summary(l: &repo::LaunchRow) -> LaunchSummary {
    LaunchSummary {
        id: l.id,
        revision_id: l.revision_id,
        delivery_channel_id: l.delivery_channel_id,
        requested_by: l.requested_by,
        status: l.status.clone(),
        manifest_hash: l.manifest_hash.as_deref().map(hex_of),
        frozen_at: l.frozen_at,
        expires_at: l.expires_at,
        version: l.version,
        created_at: l.created_at,
    }
}

/// Load launch → revision → campaign, all belonging together, else 404.
async fn launch_chain(
    tx: &mut Tx<'_>,
    launch_id: Uuid,
) -> Result<(repo::LaunchRow, repo::RevisionRow, repo::CampaignRow), AppError> {
    let Some(launch) = repo::get_launch(tx, launch_id).await? else {
        return Err(AppError::not_found("campaign launch not found"));
    };
    let revision = repo::get_revision(tx, launch.revision_id)
        .await?
        .ok_or_else(|| AppError::internal("launch revision missing"))?;
    let campaign = repo::get_campaign(tx, revision.campaign_id)
        .await?
        .ok_or_else(|| AppError::internal("launch campaign missing"))?;
    Ok((launch, revision, campaign))
}

/// Mailbox-ACL gated launch fetch used by GET + mutations.
async fn readable_launch_chain(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    launch_id: Uuid,
) -> Result<(repo::LaunchRow, repo::RevisionRow, repo::CampaignRow), AppError> {
    let (l, r, c) = launch_chain(tx, launch_id).await?;
    authz::require_mailbox(tx, ctx, c.mail_account_id, MailboxPerm::Read).await?;
    Ok((l, r, c))
}

// --------------------------------------------------------------- create

/// POST /campaigns/{id}/launches (B.3.8) — inside `idempotency::run`.
/// Returns the 202 body including the freeze job id.
pub async fn create_launch(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    campaign_id: Uuid,
    req: &CreateLaunchRequest,
) -> Result<CreateLaunchResponse, AppError> {
    let Some(campaign) = repo::get_campaign(tx, campaign_id).await? else {
        return Err(AppError::not_found("campaign not found"));
    };
    require_compose_grant(tx, ctx, campaign.mail_account_id).await?;

    let Some(revision) = repo::get_revision(tx, req.revision_id).await? else {
        return Err(AppError::not_found("revision not found"));
    };
    if revision.campaign_id != campaign_id {
        return Err(AppError::validation(
            "revision_id does not belong to this campaign",
            vec![],
        ));
    }
    if req.email_address_ids.is_empty() || req.email_address_ids.len() > MAX_RECIPIENTS {
        return Err(AppError::validation(
            "email_address_ids must contain 1..=5000 ids",
            vec![],
        ));
    }
    let steps = repo::list_steps(tx, revision.id).await?;
    if steps.len() != 1 || steps[0].step_order != 1 || steps[0].delay_hours != 0 {
        return Err(AppError::conflict(
            "this legacy revision contains follow-up automation and cannot be launched; its stored steps remain unchanged",
        ));
    }

    let rules = domain::campaigns::ScheduleRules::from_json(&revision.schedule_rules)
        .map_err(|e| AppError::internal(format!("stored schedule_rules invalid: {e}")))?;
    let expires_at = rules
        .launch_expires_at
        .unwrap_or_else(|| Utc::now() + Duration::hours(DEFAULT_LAUNCH_TTL_HOURS));

    let launch_id = repo::insert_launch(
        tx,
        revision.id,
        revision.delivery_channel_id,
        ctx.principal_id,
        expires_at,
    )
    .await?;

    // Async freeze (A.6.2): the worker locks revision+launch, snapshots
    // enrollments + step payloads, computes the manifest and creates the
    // approval request — all in one tx. Dedupe key makes re-delivery safe.
    let job_id = repo::enqueue_dedup(
        tx,
        "campaigns.freeze_launch",
        serde_json::json!({
            "campaign_launch_id": launch_id,
            "email_address_ids": req.email_address_ids,
            "variables": req.variables,
            "requester_context": requester_context_snapshot(ctx),
        }),
        Utc::now(),
        &format!("freeze_launch:{launch_id}"),
    )
    .await?;
    let source_credential_id = match ctx.auth {
        contracts::AuthMethod::ServiceCredential { credential_id } => Some(credential_id),
        _ => None,
    };
    infrastructure::repo::org::tag_job_requester(
        tx,
        job_id,
        ctx.principal_id,
        source_credential_id,
    )
    .await?;

    audit::record(
        tx,
        audited(
            ctx,
            "campaign.launch_requested",
            "campaign_launch",
            launch_id,
            serde_json::json!({
                "campaign_id": campaign_id,
                "revision_id": revision.id,
                "recipient_count": req.email_address_ids.len(),
                "job_id": job_id,
            }),
        ),
    )
    .await?;
    repo::sync_campaign_status(tx, campaign_id).await?;
    outbox::emit(
        tx,
        "campaign.launch_requested",
        "campaign_launch",
        launch_id,
        1,
        serde_json::json!({
            "campaign_id": campaign_id,
            "revision_id": revision.id,
            "job_id": job_id,
            "recipient_count": req.email_address_ids.len(),
        }),
    )
    .await?;

    Ok(CreateLaunchResponse {
        launch_id,
        status: d::launch_status::BUILDING.to_string(),
        job_id,
        version: 1,
    })
}

// ---------------------------------------------------------------- detail

/// GET /campaign-launches/{id} — read grant on the campaign mailbox.
pub async fn get_launch(
    db: &infrastructure::db::Db,
    ctx: &Ctx,
    launch_id: Uuid,
    message_cursor: Option<&str>,
    message_limit: i64,
    hmac: &[u8],
) -> Result<LaunchDetail, AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    let (launch, revision, campaign) = readable_launch_chain(&mut tx, ctx, launch_id).await?;

    let counts = repo::enrollment_counts(&mut tx, launch_id).await?;
    let steps = repo::list_steps(&mut tx, revision.id).await?;

    let filter =
        serde_json::json!({"route": "GET /campaign-launches/{id}", "launch_id": launch_id});
    let after = match message_cursor {
        Some(c) => decode_page_cursor(c, hmac, ctx.principal_id, &filter)?,
        None => None,
    };
    let rows = repo::list_launch_outbounds(&mut tx, launch_id, after, message_limit + 1).await?;
    let has_more = rows.len() as i64 > message_limit;
    let page_rows: Vec<_> = rows.into_iter().take(message_limit as usize).collect();
    let next_cursor = if has_more {
        page_rows.last().map(|row| {
            encode_page_cursor(
                row.outbound.created_at,
                row.outbound.id,
                hmac,
                ctx.principal_id,
                &filter,
            )
        })
    } else {
        None
    };
    let messages = LaunchMessagePage {
        items: page_rows
            .into_iter()
            .map(|row| LaunchMessageItem {
                outbound_message_id: row.outbound.id,
                enrollment_id: row.outbound.enrollment_id,
                step_id: row.outbound.step_id,
                step_order: row.step_order,
                to_address_snapshot: row.outbound.to_address_snapshot,
                subject: row.outbound.subject,
                body_text: row.outbound.body_text,
                body_html: row.outbound.body_html,
                status: row.outbound.status,
                not_before: row.outbound.not_before,
                payload_hash: hex_of(&row.outbound.payload_hash),
            })
            .collect(),
        next_cursor,
        has_more,
    };
    tx.commit().await?;
    Ok(LaunchDetail {
        launch: launch_summary(&launch),
        campaign_id: campaign.id,
        revision_no: revision.revision_no,
        mail_account_id: campaign.mail_account_id,
        step_count: steps.len() as i64,
        enrollments: EnrollmentSummary {
            total: counts.total,
            pending: counts.pending,
            active: counts.active,
            completed: counts.completed,
            stopped: counts.stopped,
            suppressed: counts.suppressed,
        },
        messages,
    })
}

// ---------------------------------------------------- approval requests

/// POST /campaign-launches/{id}/approval-requests (B.3.8). Caller supplies
/// the manifest it reviewed; the launch must be frozen. Re-requesting on a
/// launch whose live request already exists returns that request (the
/// `approval_launch_active_uq` index guarantees uniqueness). Returns the
/// response plus whether the request was newly created (201) or an
/// existing live one was returned (200).
pub async fn request_launch_approval(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    launch_id: Uuid,
    expected_manifest_hash: &str,
    reason: Option<&str>,
) -> Result<(LaunchApprovalRequestResponse, bool), AppError> {
    let Some(launch) = repo::get_launch_for_update(tx, launch_id).await? else {
        return Err(AppError::not_found("campaign launch not found"));
    };
    let revision = repo::get_revision(tx, launch.revision_id)
        .await?
        .ok_or_else(|| AppError::internal("launch revision missing"))?;
    let campaign = repo::get_campaign(tx, revision.campaign_id)
        .await?
        .ok_or_else(|| AppError::internal("launch campaign missing"))?;
    require_compose_grant(tx, ctx, campaign.mail_account_id).await?;

    // Expected-manifest verification (A.6.2 — what the caller saw is what
    // gets approved).
    let expected = decode_hex(expected_manifest_hash)
        .ok_or_else(|| AppError::validation("expected_manifest_hash must be sha256 hex", vec![]))?;
    let current = launch.manifest_hash.clone().unwrap_or_default();
    if expected != current {
        return Err(AppError::new(
            ErrorCode::SnapshotMismatch,
            "expected_manifest_hash does not match the frozen manifest",
        ));
    }
    match launch.status.as_str() {
        d::launch_status::FROZEN | d::launch_status::AWAITING_APPROVAL => {}
        _ => {
            return Err(AppError::conflict(format!(
                "launch is {} — approval requires a frozen launch",
                launch.status
            )));
        }
    }

    // Global policy gates — unresolved blocks reject the request outright
    // (B.3.8). Per-recipient failures were excluded at freeze.
    let channel = repo::get_channel(tx, launch.delivery_channel_id)
        .await?
        .ok_or_else(|| AppError::internal("launch channel missing"))?;
    if repo::emergency_stop_active(tx).await? {
        return Err(AppError::new(
            ErrorCode::WorkspaceEmergencyStop,
            "workspace emergency stop is active",
        ));
    }
    if let Some(reason_code) = repo::channel_blocking_reasons(&channel, false).first() {
        return Err(match reason_code.as_str() {
            "PROVIDER_POLICY_BLOCKED" => AppError::new(
                ErrorCode::ProviderPolicyBlocked,
                "delivery channel provider policy is blocked",
            ),
            _ => AppError::new(
                ErrorCode::ProviderPolicyUnverified,
                "delivery channel provider policy is not verified",
            ),
        });
    }

    // Reuse the live request when present — the launch manifest is frozen,
    // so any active request already pins this exact snapshot.
    if let Some(existing) = approvals_repo::active_request_for_launch(tx, launch_id).await? {
        return Ok((
            LaunchApprovalRequestResponse {
                approval_request_id: existing.id,
                status: existing.status,
                expires_at: existing.expires_at,
                version: existing.version,
                launch_id,
            },
            false,
        ));
    }

    let mut hash = [0u8; 32];
    hash.copy_from_slice(&current);
    let request_id = approvals_repo::insert_request(
        tx,
        ctx.principal_id,
        approvals_repo::ApprovalTarget::CampaignLaunch(launch_id),
        &hash,
        launch.expires_at,
    )
    .await?;
    if launch.status == d::launch_status::FROZEN {
        repo::transition_launch(tx, launch_id, d::launch_status::AWAITING_APPROVAL, None).await?;
    }
    let req_row = approvals_repo::get_request(tx, request_id)
        .await?
        .ok_or_else(|| AppError::internal("approval request missing after insert"))?;
    audit::record(
        tx,
        audited(
            ctx,
            "campaign.approval_requested",
            "campaign_launch",
            launch_id,
            serde_json::json!({
                "approval_request_id": request_id,
                "manifest_hash": expected_manifest_hash,
                "reason": reason,
            }),
        ),
    )
    .await?;
    Ok((
        LaunchApprovalRequestResponse {
            approval_request_id: request_id,
            status: req_row.status,
            expires_at: req_row.expires_at,
            version: req_row.version,
            launch_id,
        },
        true,
    ))
}

// ------------------------------------------------------------ pause/cancel

/// Permission rule (B.2 "담당·pause scope"): the campaign owner, an
/// admin/manager, or any principal holding `campaign.pause` (agents included
/// — §5.3 grants agents a separate pause scope for risk reduction).
fn require_pause_rights(ctx: &Ctx, campaign: &repo::CampaignRow) -> Result<(), AppError> {
    let ok = ctx.scopes.contains(contracts::Scope::CampaignPause)
        || matches!(ctx.role, contracts::Role::Admin | contracts::Role::Manager)
        || campaign.owner_principal_id == ctx.principal_id;
    if ok {
        Ok(())
    } else {
        Err(AppError::forbidden(
            "pause requires the campaign owner, admin/manager role, or campaign.pause scope",
        ))
    }
}

/// Stop the launch's live enrollments and cancel all not-yet-submitted
/// outbound rows + their dispatch jobs.
async fn quiesce_launch(
    tx: &mut Tx<'_>,
    launch_id: Uuid,
    stop_reason: &str,
) -> Result<u64, AppError> {
    let _stopped = repo::stop_launch_enrollments(tx, launch_id, stop_reason).await?;
    let outbound_ids = repo::pending_outbound_ids_for_launch(tx, launch_id).await?;
    let canceled = repo::cancel_pending_outbounds_for_launch(tx, launch_id).await?;
    let keys: Vec<String> = outbound_ids
        .iter()
        .map(|id| repo::dispatch_dedupe_key(*id))
        .collect();
    repo::cancel_queued_jobs(tx, "sending.dispatch_outbound", &keys).await?;
    Ok(canceled)
}

/// POST /campaign-launches/{id}/pause — If-Match on the launch version.
/// Pausing stops remaining sends (risk reduction); the launch keeps its
/// frozen manifest and may be canceled afterwards (no resume endpoint yet).
pub async fn pause_launch(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    launch_id: Uuid,
    expected_version: i64,
    reason: Option<&str>,
) -> Result<LaunchSummary, AppError> {
    let native_export = crate::marketing::lock_native_launch_control_tx(tx, launch_id).await?;
    let Some(launch) = repo::get_launch_for_update(tx, launch_id).await? else {
        return Err(AppError::not_found("campaign launch not found"));
    };
    let revision = repo::get_revision(tx, launch.revision_id)
        .await?
        .ok_or_else(|| AppError::internal("launch revision missing"))?;
    let campaign = repo::get_campaign(tx, revision.campaign_id)
        .await?
        .ok_or_else(|| AppError::internal("launch campaign missing"))?;
    authz::require_mailbox(tx, ctx, campaign.mail_account_id, MailboxPerm::Read).await?;
    require_pause_rights(ctx, &campaign)?;
    version_check(expected_version, launch.version)?;

    match launch.status.as_str() {
        s if d::launch_status::is_terminal(s) => {
            return Err(AppError::conflict(format!("launch is already {s}")));
        }
        d::launch_status::PAUSED => {
            // Idempotent pause — return current row.
            return Ok(launch_summary(&launch));
        }
        d::launch_status::BUILDING => {
            // A building launch cannot pause (CHECK requires a manifest for
            // every non-building state); the freeze job settles it first.
            return Err(AppError::conflict("launch is still building"));
        }
        _ => {}
    }
    crate::marketing::cancel_unsubmitted_native_export_tx(tx, native_export.as_ref()).await?;
    quiesce_launch(tx, launch_id, "campaign_paused").await?;
    repo::transition_launch(
        tx,
        launch_id,
        d::launch_status::PAUSED,
        Some(launch.version),
    )
    .await?;
    repo::sync_campaign_status(tx, campaign.id).await?;
    audit::record(
        tx,
        audited(
            ctx,
            "campaign.launch_paused",
            "campaign_launch",
            launch_id,
            serde_json::json!({"reason": reason, "campaign_id": campaign.id}),
        ),
    )
    .await?;
    let Some(updated) = repo::get_launch(tx, launch_id).await? else {
        return Err(AppError::internal("launch vanished"));
    };
    outbox::emit(
        tx,
        "campaign.launch_paused",
        "campaign_launch",
        launch_id,
        updated.version,
        serde_json::json!({"campaign_id": campaign.id, "reason": reason}),
    )
    .await?;
    Ok(launch_summary(&updated))
}

/// POST /campaign-launches/{id}/cancel — terminal transition (A.6.2 allows
/// cancel from any non-terminal, non-building state).
pub async fn cancel_launch(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    launch_id: Uuid,
    expected_version: i64,
    reason: Option<&str>,
) -> Result<LaunchSummary, AppError> {
    let native_export = crate::marketing::lock_native_launch_control_tx(tx, launch_id).await?;
    let Some(launch) = repo::get_launch_for_update(tx, launch_id).await? else {
        return Err(AppError::not_found("campaign launch not found"));
    };
    let revision = repo::get_revision(tx, launch.revision_id)
        .await?
        .ok_or_else(|| AppError::internal("launch revision missing"))?;
    let campaign = repo::get_campaign(tx, revision.campaign_id)
        .await?
        .ok_or_else(|| AppError::internal("launch campaign missing"))?;
    authz::require_mailbox(tx, ctx, campaign.mail_account_id, MailboxPerm::Read).await?;
    require_pause_rights(ctx, &campaign)?;
    version_check(expected_version, launch.version)?;

    match launch.status.as_str() {
        s if d::launch_status::is_terminal(s) => {
            if s == d::launch_status::CANCELED {
                return Ok(launch_summary(&launch)); // idempotent
            }
            return Err(AppError::conflict(format!("launch is already {s}")));
        }
        d::launch_status::BUILDING => {
            return Err(AppError::conflict("launch is still building"));
        }
        _ => {}
    }
    crate::marketing::cancel_unsubmitted_native_export_tx(tx, native_export.as_ref()).await?;
    quiesce_launch(tx, launch_id, "campaign_canceled").await?;
    repo::transition_launch(
        tx,
        launch_id,
        d::launch_status::CANCELED,
        Some(launch.version),
    )
    .await?;
    // Any still-pending approval request for this launch is revoked — a
    // canceled target can never be approved (A.6.2).
    if let Some(req) = approvals_repo::active_request_for_launch(tx, launch_id).await?
        && req.status == "pending"
    {
        approvals_repo::set_request_status(tx, req.id, "revoked", req.version).await?;
    }
    repo::sync_campaign_status(tx, campaign.id).await?;
    audit::record(
        tx,
        audited(
            ctx,
            "campaign.launch_canceled",
            "campaign_launch",
            launch_id,
            serde_json::json!({"reason": reason, "campaign_id": campaign.id}),
        ),
    )
    .await?;
    let Some(updated) = repo::get_launch(tx, launch_id).await? else {
        return Err(AppError::internal("launch vanished"));
    };
    outbox::emit(
        tx,
        "campaign.launch_canceled",
        "campaign_launch",
        launch_id,
        updated.version,
        serde_json::json!({"campaign_id": campaign.id, "reason": reason}),
    )
    .await?;
    Ok(launch_summary(&updated))
}
