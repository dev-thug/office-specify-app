//! Launch use cases (B.3.8): create (202 building + freeze job), detail,
//! approval-request creation, pause, cancel. All transitions are
//! forward-only and only touch columns the A.6.2 freeze guards allow.

use chrono::{DateTime, Duration, Utc};
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

fn execution_status_for_launch(
    launch_status: &str,
    job: Option<&repo::LaunchActivationJob>,
) -> String {
    if let Some(job) = job {
        if launch_status == d::launch_status::APPROVED && job.requested_by.is_none() {
            return "ready".to_string();
        }
        return match job.state.as_str() {
            "queued" | "retry_wait" => "queued",
            "running" => "running",
            "completed"
                if matches!(
                    launch_status,
                    d::launch_status::RUNNING | d::launch_status::COMPLETED
                ) =>
            {
                "activated"
            }
            "completed" if launch_status == d::launch_status::APPROVED => "unknown",
            "completed" => "activated",
            "dead" => "failed",
            "canceled" => "canceled",
            _ => "unknown",
        }
        .to_string();
    }
    if launch_status == d::launch_status::APPROVED {
        "ready".to_string()
    } else if matches!(
        launch_status,
        d::launch_status::RUNNING | d::launch_status::COMPLETED
    ) {
        "activated".to_string()
    } else {
        "not_ready".to_string()
    }
}

async fn launch_allowed_actions(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    launch: &repo::LaunchRow,
    campaign: &repo::CampaignRow,
    execution_status: &str,
) -> Result<Vec<String>, AppError> {
    let mut actions = vec!["read".to_string()];
    if launch.status != d::launch_status::APPROVED || execution_status != "ready" {
        return Ok(actions);
    }
    let Some(grant) = authz::mailbox_grant(tx, ctx, campaign.mail_account_id).await? else {
        return Ok(actions);
    };
    let credential_call = ctx.principal_kind == contracts::PrincipalKind::Agent
        || matches!(&ctx.auth, contracts::AuthMethod::ServiceCredential { .. });
    let principal_allowed = if credential_call {
        ctx.scopes
            .contains(contracts::Scope::CampaignSubmitApproved)
    } else {
        ctx.principal_kind == contracts::PrincipalKind::Human
    };
    if principal_allowed && (grant.can_compose || grant.can_send) {
        actions.push("execute_launch".to_string());
    }
    Ok(actions)
}

/// Check the live approved request, its immutable decision, and both sides'
/// current mailbox authority. Called before queueing and again by activation.
pub(super) async fn validate_approved_launch(
    tx: &mut Tx<'_>,
    launch: &repo::LaunchRow,
    expected_request_id: Option<Uuid>,
) -> Result<approvals_repo::ApprovalRequestRow, AppError> {
    let Some(request) = approvals_repo::active_request_for_launch(tx, launch.id).await? else {
        return Err(AppError::conflict("campaign launch has no active approval"));
    };
    if expected_request_id.is_some_and(|id| id != request.id)
        || request.action_type != "campaign_launch"
        || request.launch_id != Some(launch.id)
        || request.requested_by != launch.requested_by
        || request.status != "approved"
    {
        return Err(AppError::conflict(
            "campaign launch approval is not current",
        ));
    }
    let now = Utc::now();
    if request.expires_at <= now || launch.expires_at <= now {
        return Err(AppError::new(
            ErrorCode::ApprovalExpired,
            "campaign launch approval has expired",
        ));
    }
    let Some(manifest_hash) = launch.manifest_hash.as_ref() else {
        return Err(AppError::conflict("campaign launch has no frozen manifest"));
    };
    if manifest_hash.len() != 32 || request.snapshot_hash != *manifest_hash {
        return Err(AppError::new(
            ErrorCode::SnapshotMismatch,
            "approval snapshot does not match the frozen launch manifest",
        ));
    }

    let Some(decision) = approvals_repo::get_decision(tx, request.id).await? else {
        return Err(AppError::conflict(
            "approved campaign launch has no decision",
        ));
    };
    if decision.decision != "approved"
        || decision.version_at_decision.checked_add(1) != Some(request.version)
    {
        return Err(AppError::conflict(
            "campaign launch decision is no longer valid",
        ));
    }
    let Some((approver_status, approver_kind, approver_role)) =
        infrastructure::repo::sending::principal_state(tx, decision.decided_by).await?
    else {
        return Err(AppError::forbidden("campaign approver is unavailable"));
    };
    if approver_status != "active"
        || approver_kind != "human"
        || !matches!(approver_role.as_str(), "admin" | "manager")
    {
        return Err(AppError::forbidden(
            "campaign approver is no longer authorized",
        ));
    }
    let revision = repo::get_revision(tx, launch.revision_id)
        .await?
        .ok_or_else(|| AppError::internal("launch revision missing"))?;
    let approver_grant = infrastructure::repo::sending::grant_for_principal(
        tx,
        decision.decided_by,
        revision.mail_account_id,
    )
    .await?;
    if !matches!(approver_grant, Some((_, _, _, true, _))) {
        return Err(AppError::forbidden(
            "campaign approver no longer has mailbox approval access",
        ));
    }

    let Some((freeze_requester, source_credential_id)) =
        repo::launch_freeze_attribution(tx, launch.id).await?
    else {
        return Err(AppError::forbidden(
            "campaign launch requester attribution is unavailable",
        ));
    };
    if freeze_requester != Some(launch.requested_by) {
        return Err(AppError::forbidden(
            "campaign launch requester attribution changed",
        ));
    }
    let Some((requester_status, requester_kind, _)) =
        infrastructure::repo::sending::principal_state(tx, launch.requested_by).await?
    else {
        return Err(AppError::forbidden("campaign requester is unavailable"));
    };
    if requester_status != "active" || !matches!(requester_kind.as_str(), "human" | "agent") {
        return Err(AppError::forbidden(
            "campaign requester is no longer active",
        ));
    }
    if requester_kind == "agent" && source_credential_id.is_none() {
        return Err(AppError::forbidden(
            "campaign agent requester has no active credential",
        ));
    }
    if let Some(credential_id) = source_credential_id
        && !infrastructure::repo::auth::credential_authorizes(
            tx,
            credential_id,
            Some(launch.requested_by),
            &[contracts::Scope::CampaignCompose.as_str()],
        )
        .await?
    {
        return Err(AppError::forbidden(
            "campaign requester credential is no longer authorized",
        ));
    }
    let requester_grant = infrastructure::repo::sending::grant_for_principal(
        tx,
        launch.requested_by,
        revision.mail_account_id,
    )
    .await?;
    if !matches!(requester_grant, Some((_, true, _, _, _))) {
        return Err(AppError::forbidden(
            "campaign requester no longer has mailbox compose access",
        ));
    }
    Ok(request)
}

/// Re-check the principal and credential that explicitly queued activation.
/// False means this is an old approval-only activation job and it must finish
/// as a no-op until a human or narrowly scoped agent authorizes execution.
pub(super) async fn validate_activation_executor(
    tx: &mut Tx<'_>,
    campaign: &repo::CampaignRow,
    job: &repo::LaunchActivationJob,
) -> Result<bool, AppError> {
    let Some(executor_id) = job.requested_by else {
        return Ok(false);
    };
    let Some((status, kind, _)) =
        infrastructure::repo::sending::principal_state(tx, executor_id).await?
    else {
        return Err(AppError::forbidden(
            "campaign execution principal is unavailable",
        ));
    };
    if status != "active" {
        return Err(AppError::forbidden(
            "campaign execution principal is no longer active",
        ));
    }
    if let Some(credential_id) = job.source_credential_id {
        if !infrastructure::repo::auth::credential_authorizes(
            tx,
            credential_id,
            Some(executor_id),
            &[contracts::Scope::CampaignSubmitApproved.as_str()],
        )
        .await?
        {
            return Err(AppError::forbidden(
                "campaign execution credential is no longer authorized",
            ));
        }
    } else if kind != "human" {
        return Err(AppError::forbidden(
            "campaign agent execution requires its current credential",
        ));
    }
    if !matches!(kind.as_str(), "human" | "agent") {
        return Err(AppError::forbidden(
            "campaign execution requires a human or agent principal",
        ));
    }
    let grant = infrastructure::repo::sending::grant_for_principal(
        tx,
        executor_id,
        campaign.mail_account_id,
    )
    .await?;
    if !matches!(grant, Some((_, true, _, _, _)) | Some((_, _, true, _, _))) {
        return Err(AppError::forbidden(
            "campaign execution principal no longer has mailbox access",
        ));
    }
    Ok(true)
}

/// Queue the approved frozen launch once. Human REST callers authenticate as
/// humans; credential callers need campaign.submit_approved and the current
/// campaign mailbox compose or send grant.
pub async fn execute_launch(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    launch_id: Uuid,
    req: &contracts::campaigns::ExecuteLaunchRequest,
) -> Result<contracts::campaigns::ExecuteLaunchResponse, AppError> {
    let credential_call = ctx.principal_kind == contracts::PrincipalKind::Agent
        || matches!(&ctx.auth, contracts::AuthMethod::ServiceCredential { .. });
    if credential_call {
        authz::require_scope(ctx, contracts::Scope::CampaignSubmitApproved)?;
        if let contracts::AuthMethod::ServiceCredential { credential_id } = ctx.auth
            && !infrastructure::repo::auth::credential_authorizes(
                tx,
                credential_id,
                Some(ctx.principal_id),
                &[contracts::Scope::CampaignSubmitApproved.as_str()],
            )
            .await?
        {
            return Err(AppError::forbidden(
                "campaign execution credential is no longer authorized",
            ));
        }
    } else {
        authz::require_human(ctx)?;
    }

    let Some(launch) = repo::get_launch_for_update(tx, launch_id).await? else {
        return Err(AppError::not_found("campaign launch not found"));
    };
    version_check(req.expected_version, launch.version)?;
    if launch.status != d::launch_status::APPROVED {
        return Err(AppError::conflict(format!(
            "launch is {} and cannot be executed",
            launch.status
        )));
    }
    let expected_hash = decode_hex(&req.expected_manifest_hash)
        .filter(|hash| hash.len() == 32)
        .ok_or_else(|| AppError::validation("expected_manifest_hash must be sha256 hex", vec![]))?;
    let current_hash = launch.manifest_hash.as_deref().unwrap_or_default();
    if expected_hash.as_slice() != current_hash {
        return Err(AppError::new(
            ErrorCode::SnapshotMismatch,
            "expected_manifest_hash does not match the frozen launch manifest",
        ));
    }
    let revision = repo::get_revision(tx, launch.revision_id)
        .await?
        .ok_or_else(|| AppError::internal("launch revision missing"))?;
    let campaign = repo::get_campaign(tx, revision.campaign_id)
        .await?
        .ok_or_else(|| AppError::internal("launch campaign missing"))?;
    let caller_grant = authz::mailbox_grant(tx, ctx, campaign.mail_account_id)
        .await?
        .ok_or_else(|| {
            AppError::new(ErrorCode::MailboxAccessDenied, "no access to this mailbox")
        })?;
    if !caller_grant.can_compose && !caller_grant.can_send {
        return Err(AppError::new(
            ErrorCode::MailboxAccessDenied,
            "no access to this mailbox",
        ));
    }
    if credential_call {
        let Some((freeze_requester, source_credential_id)) =
            repo::launch_freeze_attribution(tx, launch.id).await?
        else {
            return Err(AppError::forbidden(
                "campaign launch requester attribution is unavailable",
            ));
        };
        if freeze_requester != Some(launch.requested_by) {
            return Err(AppError::forbidden(
                "campaign launch requester attribution changed",
            ));
        }
        let requester_kind =
            infrastructure::repo::sending::principal_state(tx, launch.requested_by)
                .await?
                .map(|(_, kind, _)| kind);
        if requester_kind.as_deref() == Some("agent") {
            let caller_credential_id = match ctx.auth {
                contracts::AuthMethod::ServiceCredential { credential_id } => Some(credential_id),
                _ => None,
            };
            if ctx.principal_id != launch.requested_by
                || caller_credential_id != source_credential_id
            {
                return Err(AppError::forbidden(
                    "agent execution must use the launch requester's original credential",
                ));
            }
        }
    }
    let approval = validate_approved_launch(tx, &launch, None).await?;
    let explicit_submission_at: DateTime<Utc> =
        sqlx::query_scalar("SELECT date_trunc('microseconds', now())")
            .fetch_one(tx.conn())
            .await
            .map_err(infrastructure::error::map_sqlx)?;

    let mut enqueue_or_authorize = false;
    let job = if let Some(existing) = repo::launch_activation_job(tx, launch_id).await? {
        if existing
            .approval_request_id
            .as_deref()
            .and_then(|value| Uuid::parse_str(value).ok())
            != Some(approval.id)
        {
            return Err(AppError::conflict(
                "existing activation job is bound to another approval",
            ));
        }
        if existing.requested_by.is_some() {
            validate_activation_executor(tx, &campaign, &existing).await?;
        } else if matches!(existing.state.as_str(), "queued" | "retry_wait") {
            infrastructure::repo::org::tag_job_requester(
                tx,
                existing.id,
                ctx.principal_id,
                match ctx.auth {
                    contracts::AuthMethod::ServiceCredential { credential_id } => {
                        Some(credential_id)
                    }
                    _ => None,
                },
            )
            .await?;
            enqueue_or_authorize = true;
        } else {
            return Err(AppError::conflict(
                "explicit activation job has no executor attribution",
            ));
        }
        existing.id
    } else {
        let id = repo::enqueue_dedup(
            tx,
            "campaigns.activate_launch",
            serde_json::json!({
                "campaign_launch_id": launch_id,
                "approval_request_id": approval.id,
                "explicit_submission_at": explicit_submission_at,
            }),
            Utc::now(),
            &format!("explicit_activate_launch:{launch_id}"),
        )
        .await?;
        infrastructure::repo::org::tag_job_requester(
            tx,
            id,
            ctx.principal_id,
            match ctx.auth {
                contracts::AuthMethod::ServiceCredential { credential_id } => Some(credential_id),
                _ => None,
            },
        )
        .await?;
        enqueue_or_authorize = true;
        id
    };
    if enqueue_or_authorize && !repo::bump_launch_version(tx, launch_id, launch.version).await? {
        return Err(AppError::new(
            ErrorCode::VersionMismatch,
            "campaign launch version changed during execution",
        ));
    }
    let Some(job_row) = repo::launch_activation_job(tx, launch_id).await? else {
        return Err(AppError::internal("activation job missing after enqueue"));
    };
    if job_row.explicit_submission_at.is_none() {
        return Err(AppError::conflict(
            "explicit activation job is missing its submission timestamp",
        ));
    }
    let execution_status = execution_status_for_launch(&launch.status, Some(&job_row));
    if enqueue_or_authorize {
        audit::record(
            tx,
            audited(
                ctx,
                "campaign.execution_authorized",
                "campaign_launch",
                launch_id,
                serde_json::json!({
                    "approval_request_id": approval.id,
                    "activation_job_id": job,
                    "manifest_hash": hex_of(current_hash),
                }),
            ),
        )
        .await?;
        outbox::emit(
            tx,
            "campaign.execution_queued",
            "campaign_launch",
            launch_id,
            launch.version + 1,
            serde_json::json!({
                "campaign_id": campaign.id,
                "approval_request_id": approval.id,
                "activation_job_id": job,
                "execution_status": execution_status,
            }),
        )
        .await?;
    }
    Ok(contracts::campaigns::ExecuteLaunchResponse {
        launch_id,
        launch_status: launch.status,
        execution_status,
        job_id: job,
        manifest_hash: hex_of(current_hash),
        version: launch.version + if enqueue_or_authorize { 1 } else { 0 },
    })
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
            "tracking_requested": req.tracking_requested,
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
    let activation_job = repo::launch_activation_job(&mut tx, launch_id).await?;
    let execution_status = execution_status_for_launch(&launch.status, activation_job.as_ref());
    let activation_job_id = activation_job.as_ref().map(|job| job.id);
    let allowed_actions =
        launch_allowed_actions(&mut tx, ctx, &launch, &campaign, &execution_status).await?;

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
        execution_status,
        activation_job_id,
        allowed_actions,
        messages,
    })
}

/// Read-safe campaign launch status for agents; exposes enrollment ids/counts,
/// never per-recipient addresses or frozen message bodies.
pub async fn get_launch_status(
    db: &infrastructure::db::Db,
    ctx: &Ctx,
    launch_id: Uuid,
) -> Result<contracts::campaigns::CampaignLaunchStatus, AppError> {
    super::campaign::require_read_scope_for_credentials(ctx)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let (launch, revision, campaign) = readable_launch_chain(&mut tx, ctx, launch_id).await?;
    let counts = repo::enrollment_counts(&mut tx, launch_id).await?;
    let enrollment_ids = repo::list_enrollments(&mut tx, launch_id)
        .await?
        .into_iter()
        .map(|enrollment| enrollment.id)
        .collect();
    let activation_job = repo::launch_activation_job(&mut tx, launch_id).await?;
    let execution_status = execution_status_for_launch(&launch.status, activation_job.as_ref());
    tx.commit().await?;
    Ok(contracts::campaigns::CampaignLaunchStatus {
        launch_id: launch.id,
        campaign_id: campaign.id,
        revision_id: revision.id,
        mail_account_id: campaign.mail_account_id,
        status: launch.status,
        manifest_hash: launch.manifest_hash.as_deref().map(hex_of),
        frozen_at: launch.frozen_at,
        expires_at: launch.expires_at,
        version: launch.version,
        created_at: launch.created_at,
        execution_status,
        activation_job_id: activation_job.map(|job| job.id),
        enrollments: EnrollmentSummary {
            total: counts.total,
            pending: counts.pending,
            active: counts.active,
            completed: counts.completed,
            stopped: counts.stopped,
            suppressed: counts.suppressed,
        },
        enrollment_ids,
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
            "PROVIDER_UNSUPPORTED" => {
                AppError::conflict("campaign launches require an Office SMTP channel")
            }
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
