//! Personalized sales campaign grouping, company-specific drafts, and
//! human-approved batch submission use cases.

use chrono::{DateTime, Utc};
use contracts::{
    AppError, AuthMethod, Ctx, ErrorCode, Page, PrincipalKind, Role, Scope, Violation,
    sales_campaigns::{
        AddSalesCampaignTargetRequest, CreateSalesCampaignDraftRequest, CreateSalesCampaignRequest,
        MAX_SALES_CAMPAIGN_TARGETS, QueueSalesCampaignTargetRequest,
        QueueSalesCampaignTargetResponse, SalesCampaignDraftSummary, SalesCampaignOutboundSummary,
        SalesCampaignResponse, SalesCampaignSenderResponse, SalesCampaignSenderSummary,
        SalesCampaignSummary, SalesCampaignTargetResponse, SetSalesCampaignSenderRequest,
        UpdateSalesCampaignTargetRequest,
    },
    sending::{CreateMessageDraftRequest, MessageDraftResponse},
};
use infrastructure::{
    audit::{self, AuditEntry},
    cursor::Cursor,
    db::{Db, Tx},
    repo::sales_campaigns as repo,
};
use serde_json::json;
use uuid::Uuid;

use crate::authz;

const MAX_PAGE_SIZE: i64 = 200;

fn require_campaign_scope(ctx: &Ctx) -> Result<(), AppError> {
    authz::require_scope(ctx, Scope::CampaignCompose)
}

fn require_crm_read_scope(ctx: &Ctx) -> Result<(), AppError> {
    authz::require_scope(ctx, Scope::CrmRead)
}

fn require_mail_read_scope(ctx: &Ctx) -> Result<(), AppError> {
    authz::require_scope(ctx, Scope::MailRead)
}

/// Check identity and scope before looking up an idempotency claim for a new
/// campaign create. Sender resolution remains inside the fresh-write closure.
pub fn preauthorize_sales_campaign_create_identity(ctx: &Ctx) -> Result<(), AppError> {
    require_campaign_scope(ctx)
}

/// Check identity and permission scope before examining a sender-setting
/// idempotency record. Readiness is evaluated only for a fresh write.
pub fn preauthorize_sales_campaign_sender_identity(ctx: &Ctx) -> Result<(), AppError> {
    if ctx.principal_kind != PrincipalKind::Human || ctx.role != Role::Admin {
        return Err(AppError::forbidden(
            "only a human administrator can set the workspace sender",
        ));
    }
    authz::require_scope(ctx, Scope::MailManage)
}

/// Check current sender and mailbox configuration before executing a new
/// campaign-create idempotency claim.
pub async fn preauthorize_create_sales_campaign(db: &Db, ctx: &Ctx) -> Result<(), AppError> {
    require_campaign_scope(ctx)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let profile = resolve_campaign_sender(&mut tx).await?;
    require_mail_compose(&mut tx, ctx, profile.mail_account_id).await?;
    tx.commit().await?;
    Ok(())
}

/// Check access to a campaign's immutable pinned mailbox. Used for completed
/// create replays, where the workspace default may since have changed.
pub async fn preauthorize_sales_campaign_compose(
    db: &Db,
    ctx: &Ctx,
    campaign_id: Uuid,
) -> Result<(), AppError> {
    require_campaign_scope(ctx)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let campaign = repo::get_campaign(&mut tx, campaign_id)
        .await?
        .ok_or_else(|| AppError::not_found("sales campaign not found"))?;
    require_mail_compose(&mut tx, ctx, campaign.mail_account_id).await?;
    tx.commit().await?;
    Ok(())
}

/// Check fresh CRM and mailbox access for an add-target operation before its
/// idempotency record can replay a response containing CRM identifiers.
pub async fn preauthorize_add_sales_campaign_target(
    db: &Db,
    ctx: &Ctx,
    campaign_id: Uuid,
    req: &AddSalesCampaignTargetRequest,
) -> Result<(), AppError> {
    require_campaign_scope(ctx)?;
    require_crm_read_scope(ctx)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    infrastructure::repo::sales_campaigns::lock_workspace_privacy_shared(&mut tx).await?;
    let campaign = repo::get_campaign(&mut tx, campaign_id)
        .await?
        .ok_or_else(|| AppError::not_found("sales campaign not found"))?;
    require_mail_compose(&mut tx, ctx, campaign.mail_account_id).await?;
    if !repo::crm_target_binding_exists(
        &mut tx,
        req.company_id,
        req.contact_id,
        req.email_address_id,
    )
    .await?
    {
        return Err(AppError::validation(
            "company, contact, and email address must be linked CRM records in this workspace",
            vec![violation("email_address_id", "CRM_BINDING_MISMATCH")],
        )
        .with_status(422));
    }
    tx.commit().await?;
    Ok(())
}

/// Check fresh CRM and mailbox access for an existing target before replaying
/// stage or draft responses that may contain company/contact/message data.
pub async fn preauthorize_sales_campaign_target(
    db: &Db,
    ctx: &Ctx,
    campaign_id: Uuid,
    target_id: Uuid,
) -> Result<(), AppError> {
    require_campaign_scope(ctx)?;
    require_crm_read_scope(ctx)?;
    require_mail_read_scope(ctx)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    infrastructure::repo::sales_campaigns::lock_workspace_privacy_shared(&mut tx).await?;
    let campaign = repo::get_campaign(&mut tx, campaign_id)
        .await?
        .ok_or_else(|| AppError::not_found("sales campaign not found"))?;
    require_mail_compose(&mut tx, ctx, campaign.mail_account_id).await?;
    authz::require_mailbox(
        &mut tx,
        ctx,
        campaign.mail_account_id,
        authz::MailboxPerm::Read,
    )
    .await?;
    let target = repo::get_target(&mut tx, campaign_id, target_id)
        .await?
        .ok_or_else(|| AppError::not_found("sales campaign target not found"))?;
    let crm_binding_is_live = match (
        target.company_id,
        target.contact_id,
        target.email_address_id,
    ) {
        (Some(company_id), Some(contact_id), Some(email_address_id)) => {
            repo::crm_target_binding_exists(&mut tx, company_id, contact_id, email_address_id)
                .await?
        }
        _ => false,
    };
    if !crm_binding_is_live {
        return Err(AppError::conflict(
            "the CRM target was removed; its cached response is no longer available",
        ));
    }
    tx.commit().await?;
    Ok(())
}

/// Check the exact queue actor path and current mailbox send grant before an
/// idempotent queue replay. State/version checks remain in the transactional
/// use case so an identical completed request can replay after the card moves.
pub async fn preauthorize_queue_sales_campaign_target(
    db: &Db,
    ctx: &Ctx,
    campaign_id: Uuid,
    target_id: Uuid,
    outbound_id: Uuid,
) -> Result<(), AppError> {
    require_campaign_scope(ctx)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    infrastructure::repo::sales_campaigns::lock_workspace_privacy_shared(&mut tx).await?;
    let campaign = repo::get_campaign(&mut tx, campaign_id)
        .await?
        .ok_or_else(|| AppError::not_found("sales campaign not found"))?;
    preauthorize_send_actor(&mut tx, ctx, campaign.mail_account_id).await?;
    let binding = repo::outbound_binding(&mut tx, outbound_id)
        .await?
        .ok_or_else(|| AppError::conflict("outbound message is not linked to this target"))?;
    if binding.sales_campaign_id != campaign_id
        || binding.target_id != target_id
        || binding.mail_account_id != campaign.mail_account_id
    {
        return Err(AppError::conflict(
            "outbound message is not bound to this campaign target",
        ));
    }
    if ctx.principal_kind == PrincipalKind::Agent
        && matches!(&ctx.auth, AuthMethod::ServiceCredential { credential_id }
            if binding.source_credential_id != Some(*credential_id))
    {
        return Err(AppError::forbidden(
            "an agent may queue only a draft created by its current credential",
        ));
    }
    tx.commit().await?;
    Ok(())
}

/// Validate a requested sender before executing a new settings mutation.
pub async fn preauthorize_set_sales_campaign_sender(
    db: &Db,
    ctx: &Ctx,
    req: &SetSalesCampaignSenderRequest,
) -> Result<(), AppError> {
    preauthorize_sales_campaign_sender_identity(ctx)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let profile = repo::get_sender_profile(&mut tx, req.smtp_profile_id)
        .await?
        .ok_or_else(|| AppError::not_found("SMTP profile not found"))?;
    if !profile_ready(&profile) {
        return Err(AppError::validation(
            "the default must be an active marketing SMTP sender",
            vec![violation("smtp_profile_id", "NOT_ELIGIBLE")],
        )
        .with_status(422));
    }
    authz::require_mailbox(
        &mut tx,
        ctx,
        profile.mail_account_id,
        authz::MailboxPerm::Send,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Current caller/resource authorization for a sender-setting replay. The
/// stored response describes the historical successful mutation, so replay
/// checks the explicit sender's current access without re-running its mutable
/// readiness policy.
pub async fn preauthorize_sales_campaign_sender_access(
    db: &Db,
    ctx: &Ctx,
    smtp_profile_id: Uuid,
) -> Result<(), AppError> {
    preauthorize_sales_campaign_sender_identity(ctx)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let profile = repo::get_sender_profile(&mut tx, smtp_profile_id)
        .await?
        .ok_or_else(|| AppError::not_found("SMTP profile not found"))?;
    authz::require_mailbox(
        &mut tx,
        ctx,
        profile.mail_account_id,
        authz::MailboxPerm::Send,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn preauthorize_send_actor(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    mail_account_id: Uuid,
) -> Result<(), AppError> {
    if ctx.principal_kind == PrincipalKind::Human && matches!(&ctx.auth, AuthMethod::Session { .. })
    {
        authz::require_mailbox(tx, ctx, mail_account_id, authz::MailboxPerm::Send).await?;
        return Ok(());
    }
    if ctx.principal_kind == PrincipalKind::System {
        return Err(AppError::forbidden(
            "system principals cannot queue outbound messages",
        ));
    }
    authz::require_scope(ctx, Scope::MailSubmitApproved)?;
    let Some((status, kind, _role)) =
        infrastructure::repo::sending::principal_state(tx, ctx.principal_id).await?
    else {
        return Err(AppError::forbidden("submitter principal is unavailable"));
    };
    let expected_kind = match ctx.principal_kind {
        PrincipalKind::Human => "human",
        PrincipalKind::Agent => "agent",
        PrincipalKind::System => unreachable!(),
    };
    if status != "active" || kind != expected_kind {
        return Err(AppError::forbidden("submitter principal is unavailable"));
    }
    if let AuthMethod::ServiceCredential { credential_id } = &ctx.auth {
        if !infrastructure::repo::auth::credential_authorizes(
            tx,
            *credential_id,
            Some(ctx.principal_id),
            &[Scope::MailSubmitApproved.as_str()],
        )
        .await?
        {
            return Err(AppError::forbidden(
                "approved-submit credential is no longer active or lacks its current scope",
            ));
        }
    } else if ctx.principal_kind == PrincipalKind::Agent {
        return Err(AppError::forbidden(
            "agent approved-submit requires its service credential",
        ));
    }
    let grant = authz::mailbox_grant(tx, ctx, mail_account_id).await?;
    if !grant.is_some_and(|grant| grant.can_send) {
        return Err(AppError::new(
            ErrorCode::MailboxAccessDenied,
            "approved-submit requires a current mailbox send grant",
        ));
    }
    Ok(())
}

async fn require_mail_compose(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    mail_account_id: Uuid,
) -> Result<(), AppError> {
    require_campaign_scope(ctx)?;
    authz::require_mailbox(tx, ctx, mail_account_id, authz::MailboxPerm::Compose).await?;
    Ok(())
}

fn violation(field: &str, code: &str) -> Violation {
    Violation {
        field: field.to_string(),
        code: code.to_string(),
    }
}

fn profile_ready(row: &repo::SenderProfileRow) -> bool {
    row.profile_status == "active"
        && row.integration_status == "active"
        && row.mailbox_status == "active"
        && row.channel_provider == "delivery_provider"
        && row.channel_purpose == "marketing"
        && row.channel_enabled
        && row.channel_policy_status == "allowed"
        && row.sender_status == "verified"
        && row.has_password
}

fn sender_summary(row: repo::SenderProfileRow) -> SalesCampaignSenderSummary {
    let ready = profile_ready(&row);
    SalesCampaignSenderSummary {
        smtp_profile_id: row.smtp_profile_id,
        profile_name: row.profile_name,
        mail_account_id: row.mail_account_id,
        mailbox_name: row.mailbox_name,
        delivery_channel_id: row.delivery_channel_id,
        sender_identity_id: row.sender_identity_id,
        from_address: row.from_address,
        from_name: row.from_name,
        reply_to: row.reply_to,
        status: row.profile_status,
        ready,
    }
}

fn campaign_summary(row: repo::CampaignRow) -> SalesCampaignSummary {
    SalesCampaignSummary {
        id: row.id,
        name: row.name,
        objective: row.objective,
        mail_account_id: row.mail_account_id,
        delivery_channel_id: row.delivery_channel_id,
        sender_identity_id: row.sender_identity_id,
        smtp_profile_id: row.smtp_profile_id,
        meeting_type_id: row.meeting_type_id,
        from_address: row.from_address,
        from_name: row.from_name,
        reply_to: row.reply_to,
        sender_status: row.sender_status,
        sender_ready: row.sender_ready,
        version: row.version,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

fn effective_stage(row: &repo::TargetBoardRow) -> String {
    match row.outbound_status.as_deref() {
        Some("queued" | "claimed" | "submitting") => "queued".to_string(),
        Some("accepted") => "accepted".to_string(),
        Some("failed" | "unknown" | "canceled") => "attention".to_string(),
        _ if row.company_id.is_none()
            || row.contact_id.is_none()
            || row.email_address_id.is_none() =>
        {
            "attention".to_string()
        }
        _ => row.workflow_stage.clone(),
    }
}

fn target_response(row: repo::TargetBoardRow) -> SalesCampaignTargetResponse {
    let stage = effective_stage(&row);
    let current_draft = row.draft_id.map(|draft_id| SalesCampaignDraftSummary {
        draft_id,
        subject: row.draft_subject.unwrap_or_default(),
        status: row.draft_status.unwrap_or_else(|| "discarded".to_string()),
        version: row.draft_version.unwrap_or_default(),
        outbound_message_id: row.draft_outbound_message_id,
    });
    let outbound = row
        .outbound_status
        .map(|status| SalesCampaignOutboundSummary {
            outbound_message_id: row.draft_outbound_message_id.unwrap_or_default(),
            provider_accepted: status == "accepted",
            status,
            payload_hash: row.outbound_payload_hash.unwrap_or_default(),
            not_before: row.outbound_not_before,
            approval_status: row.approval_status,
            approval_expires_at: row.approval_expires_at,
            approved_at: row.approved_at,
            accepted_at: row.accepted_at,
            failure_code: row.failure_code,
            open_count: row.open_count,
            click_count: row.click_count,
            known_reply_count: row.known_reply_count,
            last_reply_at: row.last_reply_at,
        });
    SalesCampaignTargetResponse {
        id: row.id,
        company_id: row.company_id,
        contact_id: row.contact_id,
        email_address_id: row.email_address_id,
        company_name: row.company_name,
        contact_name: row.contact_name,
        email_address: row.email_address,
        stage,
        workflow_stage: row.workflow_stage,
        version: row.version,
        current_draft,
        outbound,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

fn audit_entry(
    ctx: &Ctx,
    action: &str,
    target_type: &str,
    target_id: Uuid,
    metadata: serde_json::Value,
) -> AuditEntry {
    let mut entry = AuditEntry::new(
        ctx.principal_id,
        action,
        target_type,
        ctx.request_id.clone(),
    )
    .with_target(target_id);
    entry.metadata = metadata;
    entry
}

/// Resolve the explicit workspace default first. A stale or disabled explicit
/// default is an error; only an absent default may use a sole eligible profile.
async fn resolve_optional_campaign_sender(
    tx: &mut Tx<'_>,
) -> Result<Option<repo::SenderProfileRow>, AppError> {
    if let Some(profile_id) = repo::get_default_sender_profile_id(tx).await? {
        let profile = repo::get_sender_profile(tx, profile_id)
            .await?
            .ok_or_else(|| {
                AppError::conflict(
                    "the configured default SMTP sender is unavailable; update it in Mail accounts settings",
                )
            })?;
        if !profile_ready(&profile) {
            return Err(AppError::conflict(
                "the configured default SMTP sender is disabled or stale; update it in Mail accounts settings",
            ));
        }
        return Ok(Some(profile));
    }

    let mut eligible = repo::list_eligible_sender_profiles(tx).await?;
    match eligible.len() {
        0 => Ok(None),
        1 => Ok(Some(eligible.remove(0))),
        _ => Err(AppError::conflict(
            "multiple active marketing SMTP senders are available; select a default in Mail accounts settings",
        )),
    }
}

async fn resolve_campaign_sender(tx: &mut Tx<'_>) -> Result<repo::SenderProfileRow, AppError> {
    resolve_optional_campaign_sender(tx).await?.ok_or_else(|| {
        AppError::conflict("configure an active marketing SMTP sender in Mail accounts settings")
    })
}

fn decode_cursor(
    cursor: &str,
    hmac: &[u8],
    principal: Uuid,
    filter: &serde_json::Value,
) -> Result<Option<(DateTime<Utc>, Uuid)>, AppError> {
    let decoded = Cursor::decode(cursor, hmac, principal, &Cursor::hash_filter(filter))?;
    let (Some(created_at), Some(id)) = (decoded.sort_values.first(), decoded.sort_values.get(1))
    else {
        return Err(AppError::validation("invalid pagination cursor", vec![]));
    };
    let created_at: DateTime<Utc> = serde_json::from_value(created_at.clone())
        .map_err(|_| AppError::validation("invalid pagination cursor", vec![]))?;
    let id: Uuid = serde_json::from_value(id.clone())
        .map_err(|_| AppError::validation("invalid pagination cursor", vec![]))?;
    Ok(Some((created_at, id)))
}

fn encode_cursor(
    created_at: DateTime<Utc>,
    id: Uuid,
    hmac: &[u8],
    principal: Uuid,
    filter: &serde_json::Value,
) -> String {
    Cursor::new(
        vec![json!(created_at), json!(id)],
        Cursor::hash_filter(filter),
        principal,
    )
    .encode(hmac)
}

pub async fn list_sales_campaigns(
    db: &Db,
    ctx: &Ctx,
    cursor: Option<&str>,
    limit: i64,
    hmac: &[u8],
) -> Result<(Vec<SalesCampaignSummary>, Page), AppError> {
    require_campaign_scope(ctx)?;
    require_mail_read_scope(ctx)?;
    let filter = json!({"route":"GET /sales-campaigns"});
    let after = match cursor {
        Some(cursor) => decode_cursor(cursor, hmac, ctx.principal_id, &filter)?,
        None => None,
    };
    let limit = limit.clamp(1, MAX_PAGE_SIZE);
    let mut tx = db.begin(ctx.workspace_id).await?;
    let rows =
        repo::list_campaigns_for_principal(&mut tx, ctx.principal_id, after, limit + 1).await?;
    let has_more = rows.len() as i64 > limit;
    let page_rows: Vec<_> = rows.into_iter().take(limit as usize).collect();
    let next_cursor = if has_more {
        page_rows
            .last()
            .map(|row| encode_cursor(row.created_at, row.id, hmac, ctx.principal_id, &filter))
    } else {
        None
    };
    let items = page_rows.into_iter().map(campaign_summary).collect();
    tx.commit().await?;
    Ok((
        items,
        Page {
            next_cursor,
            has_more,
        },
    ))
}

pub async fn get_sales_campaign_sender(
    db: &Db,
    ctx: &Ctx,
) -> Result<SalesCampaignSenderResponse, AppError> {
    require_campaign_scope(ctx)?;
    require_mail_read_scope(ctx)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let sender = match resolve_optional_campaign_sender(&mut tx).await? {
        Some(profile) => {
            let grant = authz::mailbox_grant(&mut tx, ctx, profile.mail_account_id).await?;
            grant
                .is_some_and(|grant| grant.can_read)
                .then(|| sender_summary(profile))
        }
        None => None,
    };
    tx.commit().await?;
    Ok(SalesCampaignSenderResponse { sender })
}

pub async fn set_sales_campaign_sender(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    req: &SetSalesCampaignSenderRequest,
) -> Result<SalesCampaignSenderResponse, AppError> {
    if ctx.principal_kind != PrincipalKind::Human || ctx.role != Role::Admin {
        return Err(AppError::forbidden(
            "only a human administrator can set the workspace sender",
        ));
    }
    authz::require_scope(ctx, Scope::MailManage)?;
    let profile = repo::get_sender_profile(tx, req.smtp_profile_id)
        .await?
        .ok_or_else(|| AppError::not_found("SMTP profile not found"))?;
    if !profile_ready(&profile) {
        return Err(AppError::validation(
            "the default must be an active marketing SMTP sender",
            vec![violation("smtp_profile_id", "NOT_ELIGIBLE")],
        )
        .with_status(422));
    }
    authz::require_mailbox(tx, ctx, profile.mail_account_id, authz::MailboxPerm::Send).await?;
    repo::set_default_sender_profile(tx, profile.smtp_profile_id, ctx.principal_id).await?;
    // Campaigns that have not queued or sent anything follow the new default.
    let repinned = repo::repin_unsent_campaigns(tx, &profile).await?;
    audit::record(
        tx,
        audit_entry(
            ctx,
            "sales_campaign.sender_default_set",
            "smtp_profile",
            profile.smtp_profile_id,
            json!({"mail_account_id":profile.mail_account_id,"delivery_channel_id":profile.delivery_channel_id,"campaigns_repinned":repinned}),
        ),
    )
    .await?;
    Ok(SalesCampaignSenderResponse {
        sender: Some(sender_summary(profile)),
    })
}

pub async fn create_sales_campaign(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    req: &CreateSalesCampaignRequest,
) -> Result<SalesCampaignSummary, AppError> {
    require_campaign_scope(ctx)?;
    let name = req.name.trim();
    if name.is_empty() || name.len() > 200 {
        return Err(AppError::validation(
            "name must contain 1 to 200 characters",
            vec![violation("name", "LENGTH")],
        )
        .with_status(422));
    }
    if req
        .objective
        .as_ref()
        .is_some_and(|objective| objective.len() > 10_000)
    {
        return Err(AppError::validation(
            "objective must be at most 10000 characters",
            vec![violation("objective", "LENGTH")],
        )
        .with_status(422));
    }
    if let Some(meeting_type_id) = req.meeting_type_id {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM meeting_types
              WHERE workspace_id = $1 AND id = $2 AND status = 'active')",
        )
        .bind(ctx.workspace_id)
        .bind(meeting_type_id)
        .fetch_one(tx.conn())
        .await
        .map_err(infrastructure::error::map_sqlx)?;
        if !exists {
            return Err(AppError::validation(
                "meeting_type_id must identify an active meeting type in this workspace",
                vec![violation("meeting_type_id", "NOT_FOUND")],
            )
            .with_status(422));
        }
    }
    let profile = resolve_campaign_sender(tx).await?;
    require_mail_compose(tx, ctx, profile.mail_account_id).await?;
    let campaign_id = repo::insert_campaign(
        tx,
        name,
        req.objective.as_deref(),
        &profile,
        req.meeting_type_id,
        ctx.principal_id,
    )
    .await?;
    audit::record(
        tx,
        audit_entry(
            ctx,
            "sales_campaign.created",
            "sales_campaign",
            campaign_id,
            json!({
                "mail_account_id":profile.mail_account_id,
                "delivery_channel_id":profile.delivery_channel_id,
                "sender_identity_id":profile.sender_identity_id,
                "smtp_profile_id":profile.smtp_profile_id,
            }),
        ),
    )
    .await?;
    let row = repo::get_campaign(tx, campaign_id)
        .await?
        .ok_or_else(|| AppError::internal("created sales campaign disappeared"))?;
    Ok(campaign_summary(row))
}

/// Re-pin an existing campaign to the workspace's current default sender
/// (e.g. after switching to a Zoho alias). Allowed only while no message of
/// the campaign has been queued or sent, and only within the same mailbox.
pub async fn refresh_sales_campaign_sender(
    db: &Db,
    ctx: &Ctx,
    campaign_id: Uuid,
) -> Result<SalesCampaignSummary, AppError> {
    require_campaign_scope(ctx)?;
    require_crm_read_scope(ctx)?;
    require_mail_read_scope(ctx)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    infrastructure::repo::sales_campaigns::lock_workspace_privacy_shared(&mut tx).await?;
    let campaign = repo::lock_campaign(&mut tx, campaign_id)
        .await?
        .ok_or_else(|| AppError::not_found("sales campaign not found"))?;
    require_mail_compose(&mut tx, ctx, campaign.mail_account_id).await?;
    let profile = resolve_campaign_sender(&mut tx).await?;
    require_mail_compose(&mut tx, ctx, profile.mail_account_id).await?;
    if profile.mail_account_id != campaign.mail_account_id {
        return Err(AppError::conflict(
            "the default sender uses a different mailbox; create a new campaign instead",
        ));
    }
    if profile.smtp_profile_id != campaign.smtp_profile_id {
        if repo::campaign_has_outbound(&mut tx, campaign_id).await? {
            return Err(AppError::conflict(
                "this campaign already has queued or sent messages; create a new campaign to use another sender",
            ));
        }
        repo::repin_campaign_sender(&mut tx, campaign_id, &profile).await?;
        audit::record(
            &mut tx,
            audit_entry(
                ctx,
                "sales_campaign.sender_refreshed",
                "sales_campaign",
                campaign_id,
                json!({
                    "smtp_profile_id":profile.smtp_profile_id,
                    "delivery_channel_id":profile.delivery_channel_id,
                    "sender_identity_id":profile.sender_identity_id,
                }),
            ),
        )
        .await?;
    }
    let row = repo::get_campaign(&mut tx, campaign_id)
        .await?
        .ok_or_else(|| AppError::internal("sales campaign disappeared"))?;
    tx.commit().await?;
    Ok(campaign_summary(row))
}

pub async fn get_sales_campaign(
    db: &Db,
    ctx: &Ctx,
    campaign_id: Uuid,
) -> Result<SalesCampaignResponse, AppError> {
    require_campaign_scope(ctx)?;
    require_crm_read_scope(ctx)?;
    require_mail_read_scope(ctx)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    infrastructure::repo::sales_campaigns::lock_workspace_privacy_shared(&mut tx).await?;
    let campaign = repo::get_campaign(&mut tx, campaign_id)
        .await?
        .ok_or_else(|| AppError::not_found("sales campaign not found"))?;
    authz::require_mailbox(
        &mut tx,
        ctx,
        campaign.mail_account_id,
        authz::MailboxPerm::Read,
    )
    .await?;
    let targets = repo::list_target_board_rows(&mut tx, campaign_id)
        .await?
        .into_iter()
        .map(target_response)
        .collect();
    tx.commit().await?;
    Ok(SalesCampaignResponse {
        campaign: campaign_summary(campaign),
        targets,
    })
}

pub async fn add_sales_campaign_target(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    campaign_id: Uuid,
    req: &AddSalesCampaignTargetRequest,
) -> Result<SalesCampaignTargetResponse, AppError> {
    require_campaign_scope(ctx)?;
    require_crm_read_scope(ctx)?;
    infrastructure::repo::sales_campaigns::lock_workspace_privacy_shared(tx).await?;
    let campaign = repo::lock_campaign(tx, campaign_id)
        .await?
        .ok_or_else(|| AppError::not_found("sales campaign not found"))?;
    require_mail_compose(tx, ctx, campaign.mail_account_id).await?;
    let count = repo::count_targets(tx, campaign_id).await?;
    if count >= MAX_SALES_CAMPAIGN_TARGETS as i64 {
        return Err(AppError::validation(
            "a personalized sales campaign can contain at most 200 targets",
            vec![violation("targets", "MAX_TARGETS")],
        )
        .with_status(422));
    }
    let Some(target_id) = repo::insert_target(
        tx,
        campaign_id,
        req.company_id,
        req.contact_id,
        req.email_address_id,
    )
    .await?
    else {
        return Err(AppError::validation(
            "company, contact, and email address must be linked CRM records in this workspace",
            vec![violation("email_address_id", "CRM_BINDING_MISMATCH")],
        )
        .with_status(422));
    };
    repo::touch_campaign(tx, campaign_id).await?;
    audit::record(
        tx,
        audit_entry(
            ctx,
            "sales_campaign.target_added",
            "sales_campaign_target",
            target_id,
            json!({"sales_campaign_id":campaign_id}),
        ),
    )
    .await?;
    let row = repo::get_target_board_row(tx, campaign_id, target_id)
        .await?
        .ok_or_else(|| AppError::internal("created target disappeared"))?;
    Ok(target_response(row))
}

pub async fn update_sales_campaign_target_stage(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    campaign_id: Uuid,
    target_id: Uuid,
    req: &UpdateSalesCampaignTargetRequest,
) -> Result<SalesCampaignTargetResponse, AppError> {
    require_campaign_scope(ctx)?;
    require_crm_read_scope(ctx)?;
    require_mail_read_scope(ctx)?;
    infrastructure::repo::sales_campaigns::lock_workspace_privacy_shared(tx).await?;
    let campaign = repo::lock_campaign(tx, campaign_id)
        .await?
        .ok_or_else(|| AppError::not_found("sales campaign not found"))?;
    require_mail_compose(tx, ctx, campaign.mail_account_id).await?;
    authz::require_mailbox(tx, ctx, campaign.mail_account_id, authz::MailboxPerm::Read).await?;
    let target = repo::lock_target(tx, campaign_id, target_id)
        .await?
        .ok_or_else(|| AppError::not_found("sales campaign target not found"))?;
    if target.version != req.expected_target_version {
        return Err(AppError::new(
            ErrorCode::VersionMismatch,
            format!(
                "target version mismatch: expected {}, current {}",
                req.expected_target_version, target.version
            ),
        ));
    }
    if !matches!(req.stage.as_str(), "research" | "draft" | "review") {
        return Err(AppError::validation(
            "stage must be research, draft, or review",
            vec![violation("stage", "NOT_EDITABLE")],
        )
        .with_status(422));
    }
    if matches!(req.stage.as_str(), "draft" | "review") && target.current_draft_id.is_none() {
        return Err(AppError::validation(
            "a target needs a draft before it can move to draft or review",
            vec![violation("stage", "DRAFT_REQUIRED")],
        )
        .with_status(422));
    }
    if let Some(status) = repo::current_draft_outbound_status(tx, &target).await?
        && status != "draft"
    {
        return Err(AppError::conflict(
            "queued and completed delivery columns are derived and cannot be moved manually",
        ));
    }
    if !repo::update_target_stage(
        tx,
        campaign_id,
        target_id,
        req.expected_target_version,
        &req.stage,
    )
    .await?
    {
        return Err(AppError::new(
            ErrorCode::VersionMismatch,
            "target version changed",
        ));
    }
    repo::touch_campaign(tx, campaign_id).await?;
    audit::record(
        tx,
        audit_entry(
            ctx,
            "sales_campaign.target_stage_changed",
            "sales_campaign_target",
            target_id,
            json!({"sales_campaign_id":campaign_id,"stage":req.stage}),
        ),
    )
    .await?;
    let row = repo::get_target_board_row(tx, campaign_id, target_id)
        .await?
        .ok_or_else(|| AppError::internal("updated target disappeared"))?;
    Ok(target_response(row))
}

pub async fn create_sales_campaign_target_draft<'db>(
    mut tx: Tx<'db>,
    ctx: &Ctx,
    campaign_id: Uuid,
    target_id: Uuid,
    req: &CreateSalesCampaignDraftRequest,
) -> Result<(Tx<'db>, MessageDraftResponse), AppError> {
    require_campaign_scope(ctx)?;
    require_crm_read_scope(ctx)?;
    require_mail_read_scope(ctx)?;
    infrastructure::repo::sales_campaigns::lock_workspace_privacy_shared(&mut tx).await?;
    let campaign = repo::lock_campaign(&mut tx, campaign_id)
        .await?
        .ok_or_else(|| AppError::not_found("sales campaign not found"))?;
    require_mail_compose(&mut tx, ctx, campaign.mail_account_id).await?;
    authz::require_mailbox(
        &mut tx,
        ctx,
        campaign.mail_account_id,
        authz::MailboxPerm::Read,
    )
    .await?;
    let target = repo::lock_target(&mut tx, campaign_id, target_id)
        .await?
        .ok_or_else(|| AppError::not_found("sales campaign target not found"))?;
    if target.company_id.is_none()
        || target.contact_id.is_none()
        || target.email_address_id.is_none()
    {
        return Err(AppError::conflict(
            "the CRM contact or email was removed; restore the CRM binding before drafting",
        ));
    }
    if let Some(status) = repo::current_draft_outbound_status(&mut tx, &target).await? {
        match status.as_str() {
            "queued" | "claimed" | "submitting" => {
                return Err(AppError::conflict(
                    "cancel queued work before replacing the current draft",
                ));
            }
            "unknown" => {
                return Err(AppError::conflict(
                    "reconcile the unknown send before creating another draft",
                ));
            }
            "accepted" if !req.explicit_follow_up => {
                return Err(AppError::conflict(
                    "creating a draft after provider acceptance requires explicit_follow_up",
                ));
            }
            _ => {}
        }
    }
    let email_address_id = target
        .email_address_id
        .ok_or_else(|| AppError::conflict("the target email address is unavailable"))?;
    let draft_request = CreateMessageDraftRequest {
        mail_account_id: campaign.mail_account_id,
        to_email_address_id: email_address_id,
        purpose: "marketing".to_string(),
        subject: req.subject.clone(),
        body_text: req.body_text.clone(),
        body_html: req.body_html.clone(),
        template_version_id: req.template_version_id,
        variables: req.variables.clone(),
        thread_id: None,
        reply_to_message_id: None,
        expected_last_inbound_id: None,
        visibility: Some("mailbox".to_string()),
    };
    let (mut tx, mut draft) = crate::sending::create_draft_in_tx(tx, ctx, &draft_request).await?;
    // The linked-submit order is campaign → target → draft → address. The
    // shared privacy gate keeps erasure out of this transaction while the
    // newly inserted draft takes its address FK lock.
    if !infrastructure::repo::compliance_core::lock_email_address(&mut tx, email_address_id).await?
    {
        return Err(AppError::conflict(
            "the target email address is unavailable",
        ));
    }
    repo::insert_target_draft(
        &mut tx,
        campaign_id,
        target_id,
        campaign.mail_account_id,
        draft.draft_id,
    )
    .await?;
    repo::set_current_target_draft(&mut tx, campaign_id, target_id, draft.draft_id).await?;
    repo::touch_campaign(&mut tx, campaign_id).await?;
    draft.sales_campaign_id = Some(campaign_id);
    draft.sales_target_id = Some(target_id);
    audit::record(
        &mut tx,
        audit_entry(
            ctx,
            "sales_campaign.target_draft_created",
            "sales_campaign_target",
            target_id,
            json!({"sales_campaign_id":campaign_id,"draft_id":draft.draft_id}),
        ),
    )
    .await?;
    Ok((tx, draft))
}

pub async fn queue_sales_campaign_target(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    campaign_id: Uuid,
    target_id: Uuid,
    req: &QueueSalesCampaignTargetRequest,
) -> Result<QueueSalesCampaignTargetResponse, AppError> {
    require_campaign_scope(ctx)?;
    infrastructure::repo::sales_campaigns::lock_workspace_privacy_shared(tx).await?;
    let campaign = repo::lock_campaign(tx, campaign_id)
        .await?
        .ok_or_else(|| AppError::not_found("sales campaign not found"))?;
    let target = repo::lock_target(tx, campaign_id, target_id)
        .await?
        .ok_or_else(|| AppError::not_found("sales campaign target not found"))?;
    if target.version != req.expected_target_version {
        return Err(AppError::new(
            ErrorCode::VersionMismatch,
            format!(
                "target version mismatch: expected {}, current {}",
                req.expected_target_version, target.version
            ),
        ));
    }
    if target.current_draft_id.is_none()
        || target.company_id.is_none()
        || target.contact_id.is_none()
        || target.email_address_id.is_none()
    {
        return Err(AppError::conflict(
            "the target needs a current CRM-bound draft before it can be queued",
        ));
    }
    let binding = repo::outbound_binding(tx, req.outbound_message_id)
        .await?
        .ok_or_else(|| AppError::conflict("outbound message is not linked to this target"))?;
    if binding.sales_campaign_id != campaign_id
        || binding.target_id != target_id
        || !binding.is_current
        || binding.current_draft_id != target.current_draft_id
        || binding.source_draft_id != target.current_draft_id.unwrap_or_default()
        || !binding.recipient_available
        || binding.mail_account_id != campaign.mail_account_id
        || binding.delivery_channel_id != campaign.delivery_channel_id
        || binding.sender_identity_id != campaign.sender_identity_id
        || binding.email_address_id != target.email_address_id
        || binding.outbound_mail_account_id != campaign.mail_account_id
        || binding.outbound_delivery_channel_id != campaign.delivery_channel_id
        || binding.outbound_sender_identity_id != campaign.sender_identity_id
        || binding.outbound_email_address_id != target.email_address_id.unwrap_or_default()
    {
        return Err(AppError::conflict(
            "outbound message no longer matches the current target draft and sender",
        ));
    }
    if binding.status != "draft" {
        return Err(AppError::conflict(
            "only an unsubmitted current outbound message can be queued",
        ));
    }

    let requested_batch = next_sales_batch_boundary(Utc::now());
    let submission = crate::sending::submit_scheduled_approved_in_tx(
        tx,
        ctx,
        req.outbound_message_id,
        &req.expected_payload_hash,
        requested_batch,
    )
    .await?;
    let persisted = repo::outbound_binding(tx, req.outbound_message_id)
        .await?
        .ok_or_else(|| AppError::internal("queued campaign outbound binding disappeared"))?;
    let batch_at = persisted
        .not_before
        .ok_or_else(|| AppError::internal("queued campaign outbound has no scheduled time"))?;
    repo::touch_target(tx, campaign_id, target_id).await?;
    repo::touch_campaign(tx, campaign_id).await?;
    audit::record(
        tx,
        audit_entry(
            ctx,
            "sales_campaign.target_queued",
            "sales_campaign_target",
            target_id,
            json!({
                "sales_campaign_id":campaign_id,
                "outbound_message_id":req.outbound_message_id,
                "batch_at":batch_at,
            }),
        ),
    )
    .await?;
    Ok(QueueSalesCampaignTargetResponse {
        submission,
        batch_at,
    })
}

pub fn next_sales_batch_boundary(now: DateTime<Utc>) -> DateTime<Utc> {
    let seconds = now.timestamp();
    let next = seconds.div_euclid(600).saturating_add(1) * 600;
    DateTime::from_timestamp(next, 0).expect("UTC timestamp is in supported range")
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};

    use super::next_sales_batch_boundary;

    #[test]
    fn batch_boundary_is_strictly_next_utc_ten_minute_epoch() {
        let cases = [
            (1_791_576_000, 1_791_576_600), // exactly on a boundary
            (1_791_576_001, 1_791_576_600),
            (1_791_576_599, 1_791_576_600),
            (1_791_576_600, 1_791_577_200),
        ];
        for (input, expected) in cases {
            let instant = DateTime::<Utc>::from_timestamp(input, 0).unwrap();
            assert_eq!(next_sales_batch_boundary(instant).timestamp(), expected);
        }
    }
}
