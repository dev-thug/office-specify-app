//! Office-owned snapshots for one immediate Zoho Campaigns bulk send.
//!
//! Every provider mutation has a durable, committed operation claim before
//! the HTTP request. Unknown results remain blocked until a provider read
//! reconciles them; this module never retries a Campaigns write.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use chrono::{DateTime, Duration, Utc};
use contracts::{
    AppError, AuthMethod, Ctx, ErrorCode, PrincipalKind, Role, Scope,
    marketing::{
        MarketingExportContentResponse, MarketingExportResponse, PrepareMarketingExportRequest,
        SendMarketingExportRequest,
    },
};
use domain::marketing::{MarketingExportState as ExportState, OperationClaim, OperationResolution};
use infrastructure::{
    audit::{self, AuditEntry},
    config::Config,
    crypto,
    db::{Db, Tx},
    jobs, outbox,
    providers::zoho_campaigns::{
        http::CampaignsHttp,
        operations::{
            CampaignContactStatus, CampaignCreateRequest, CampaignMutationOutcome,
            CampaignRecipientAction, CampaignSignupForm, CampaignType, CampaignsOperations,
            CampaignsOperationsError, CampaignsSortOrder, ContactInput, CreateMailingListRequest,
        },
    },
    repo::{
        approvals as approvals_repo, campaigns as campaigns_repo, compliance as compliance_repo,
        compliance_core, marketing as marketing_repo, marketing_exports as exports_repo,
        sending as sending_repo,
    },
};
use serde_json::{Value, json};
use uuid::Uuid;

use super::content::{
    CampaignContentInput, CampaignFieldMapping, RecipientVariables, transform_campaign_content,
};

const MAX_EXPORT_RECIPIENTS: usize = 2_000;
const MAX_CONTACT_WRITES_PER_SYNC: usize = 25;
const PRIOR_SCAN_EPOCH_REPORT_KEY: &str = "prior_scan_epoch_ms";
const PRIOR_SCAN_MORE_REPORT_KEY: &str = "prior_scan_more";
const SEND_PRIOR_SCAN_EPOCH_REPORT_KEY: &str = "send_scan_epoch_ms";
const SEND_PRIOR_SCAN_MORE_REPORT_KEY: &str = "send_scan_more";
const PRIOR_SCAN_RESUME_WINDOW: Duration = Duration::minutes(5);
const EXPORT_TTL: Duration = Duration::days(7);
const EXPORT_POLL_INTERVAL: Duration = Duration::minutes(5);
const CAMPAIGNS_CONTENT_ROUTE: &str = "/public/v1/marketing-content";
const BODY_READBACK_NOTE: &str = "Zoho Campaigns details do not return the saved HTML body; Office remains the immutable source, and Campaigns metadata/list targeting are verified.";
const PROVIDER_EDIT_NOTE: &str = "Office verifies Campaigns sender, subject, list, and mapped contact fields immediately before submission; Zoho does not lock out console edits during the final request.";

#[derive(Debug, Clone)]
struct SourceRecipient {
    email_address_id: Uuid,
    outbound_message_id: Uuid,
    normalized_address: String,
    variables: RecipientVariables,
}

fn export_hash(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn hex_bytes(raw: &[u8], field: &str) -> Result<String, AppError> {
    if raw.len() != 32 {
        return Err(AppError::internal(format!(
            "stored {field} hash has an invalid length"
        )));
    }
    Ok(export_hash(raw))
}

fn decode_hex_hash(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let decoded = (0..value.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&value[index..index + 2], 16).ok())
        .collect::<Option<Vec<_>>>()?;
    decoded.try_into().ok()
}

fn response(
    row: exports_repo::MarketingExportRow,
    recipients: &[exports_repo::MarketingExportRecipientRow],
) -> Result<MarketingExportResponse, AppError> {
    Ok(MarketingExportResponse {
        id: row.id,
        launch_id: row.launch_id,
        connection_id: row.connection_id,
        state: row.state,
        version: row.version,
        source_manifest_hash: hex_bytes(&row.source_manifest_hash, "source manifest")?,
        export_hash: hex_bytes(&row.export_hash, "export")?,
        content_hash: hex_bytes(&row.content_hash, "content")?,
        recipient_count: i32::try_from(row.recipient_count)
            .map_err(|_| AppError::internal("marketing export recipient count is out of range"))?,
        active_count: i32::try_from(
            recipients
                .iter()
                .filter(|recipient| recipient.sync_status == "active")
                .count(),
        )
        .map_err(|_| AppError::internal("marketing export active count is out of range"))?,
        pending_count: i32::try_from(
            recipients
                .iter()
                .filter(|recipient| {
                    matches!(
                        recipient.sync_status.as_str(),
                        "pending" | "pending_confirmation"
                    )
                })
                .count(),
        )
        .map_err(|_| AppError::internal("marketing export pending count is out of range"))?,
        excluded_count: i32::try_from(
            recipients
                .iter()
                .filter(|recipient| {
                    matches!(
                        recipient.sync_status.as_str(),
                        "excluded" | "unsubscribed" | "bounced"
                    )
                })
                .count(),
        )
        .map_err(|_| AppError::internal("marketing export excluded count is out of range"))?,
        sender_address: row.sender_address,
        sender_name: row.sender_name,
        subject: row.subject,
        provider_list_key: row.provider_list_key,
        topic_id: row.topic_id,
        provider_campaign_key: row.campaign_key,
        last_checked_at: row.last_checked_at,
        expires_at: row.expires_at,
        created_at: row.created_at,
        report: row.report,
    })
}

// Office's normal contact API creates syntactically checked addresses as
// `unknown`; it has no deliverability-verification endpoint. Consent remains
// mandatory and provider-confirmed exact list membership is checked before
// submission. Known invalid/risky (or unexpected) statuses stay blocked.
fn recipient_address_status_allowed(status: &str) -> bool {
    matches!(status, "unknown" | "verified")
}

fn campaigns_error(error: CampaignsOperationsError) -> AppError {
    match error {
        CampaignsOperationsError::ReauthRequired => AppError::new(
            ErrorCode::IntegrationReauthRequired,
            "Zoho Campaigns authorization must be renewed",
        ),
        CampaignsOperationsError::RateLimited { .. } => AppError::new(
            ErrorCode::RateLimited,
            "Zoho Campaigns rate limited the request",
        ),
        CampaignsOperationsError::Rejected { code } => AppError::conflict(format!(
            "Zoho Campaigns rejected the operation with code {}",
            safe_provider_code(&code)
        )),
        CampaignsOperationsError::Transport
        | CampaignsOperationsError::HttpStatus { .. }
        | CampaignsOperationsError::InvalidResponse
        | CampaignsOperationsError::ResponseTooLarge
        | CampaignsOperationsError::PaginationLimitExceeded => AppError::dependency_unavailable(
            "Zoho Campaigns could not verify the requested operation",
        ),
        CampaignsOperationsError::NotConfigured
        | CampaignsOperationsError::InvalidConfiguration => {
            AppError::dependency_unavailable("Zoho Campaigns operations are not configured")
        }
        CampaignsOperationsError::InvalidRequest | CampaignsOperationsError::RequestTooLarge => {
            AppError::validation("Campaigns operation request is invalid", vec![])
        }
    }
}

fn safe_provider_code(value: &str) -> String {
    let normalized: String = value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
        .take(32)
        .collect();
    if normalized.is_empty() {
        "UNKNOWN".into()
    } else {
        normalized
    }
}

fn validate_immediate_schedule(rules_json: &Value) -> Result<(), AppError> {
    let rules = domain::campaigns::ScheduleRules::from_json(rules_json)
        .map_err(|_| AppError::validation("invalid Office launch schedule", vec![]))?;
    let every_day = rules.weekdays.is_empty() || rules.weekdays == (1..=7).collect::<Vec<_>>();
    let (start, end) = rules.window();
    let earliest = chrono::NaiveTime::from_hms_opt(0, 0, 0).expect("valid midnight");
    let latest = chrono::NaiveTime::from_hms_opt(23, 59, 0).expect("valid end of day");
    if rules.launch_not_before.is_some()
        || !every_day
        || rules.window_start.is_some_and(|value| value != earliest)
        || rules.window_end.is_some_and(|value| value < latest)
        || rules.internal_daily_cap.is_some()
        || rules.minimum_gap_seconds.is_some_and(|value| value > 0)
    {
        return Err(AppError::validation(
            "Zoho Campaigns Free supports only an immediate, unrestricted Office launch",
            vec![],
        ));
    }
    let _ = (start, end);
    Ok(())
}

fn require_admin(ctx: &Ctx) -> Result<(), AppError> {
    if ctx.principal_kind != PrincipalKind::Human || ctx.role != Role::Admin {
        return Err(AppError::forbidden(
            "human administrator required for marketing exports",
        ));
    }
    if !matches!(ctx.auth, AuthMethod::Session { .. }) {
        return Err(AppError::forbidden(
            "browser session required for marketing exports",
        ));
    }
    Ok(())
}

fn hmac_request(config: &Config, value: &impl serde::Serialize) -> Result<[u8; 32], AppError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|_| AppError::internal("could not fingerprint Campaigns operation"))?;
    Ok(crypto::hmac_sha256(&config.hmac_key, &bytes))
}

async fn enqueue_export_poll_in_tx(
    tx: &mut Tx<'_>,
    export_id: Uuid,
    delay: Duration,
) -> Result<(), AppError> {
    let available_at = Utc::now() + delay;
    let bucket = available_at.timestamp() / EXPORT_POLL_INTERVAL.num_seconds();
    jobs::enqueue(
        tx,
        "marketing.poll_export",
        1,
        json!({"export_id":export_id}),
        available_at,
        Some(&format!("marketing.poll_export:{export_id}:{bucket}")),
    )
    .await?;
    Ok(())
}

async fn account_and_scopes(
    db: &Db,
    ctx: &Ctx,
    connection_id: Uuid,
    required: &[&str],
) -> Result<(), AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    verify_connection_scopes_in_tx(&mut tx, connection_id, required).await?;
    tx.commit().await
}

async fn verify_connection_scopes_in_tx(
    tx: &mut Tx<'_>,
    connection_id: Uuid,
    required: &[&str],
) -> Result<(), AppError> {
    let connection = marketing_repo::get_connection(tx, connection_id)
        .await?
        .ok_or_else(|| AppError::not_found("Campaigns connection not found"))?;
    if connection.status != "active" {
        return Err(AppError::new(
            ErrorCode::IntegrationReauthRequired,
            "Campaigns connection is not active",
        ));
    }
    let scopes = marketing_repo::get_connection_scopes(tx, connection_id).await?;
    if required
        .iter()
        .any(|scope| !scopes.iter().any(|granted| granted == scope))
    {
        return Err(AppError::new(
            ErrorCode::IntegrationReauthRequired,
            "Campaigns connection does not have the required delivery scopes",
        ));
    }
    Ok(())
}

async fn provider_access(
    db: &Db,
    ctx: &Ctx,
    config: &Config,
    oauth: Option<&CampaignsHttp>,
    connection_id: Uuid,
) -> Result<infrastructure::providers::zoho_campaigns::http::OAuthTokens, AppError> {
    let oauth = oauth.ok_or_else(|| {
        AppError::dependency_unavailable("Zoho Campaigns OAuth is not configured")
    })?;
    super::access_tokens(db, ctx, config, oauth, connection_id).await
}

async fn first_export(
    db: &Db,
    ctx: &Ctx,
    export_id: Uuid,
) -> Result<
    (
        exports_repo::MarketingExportRow,
        Vec<exports_repo::MarketingExportRecipientRow>,
    ),
    AppError,
> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    let row = exports_repo::get_export(&mut tx, export_id)
        .await?
        .ok_or_else(|| AppError::not_found("marketing export not found"))?;
    let recipients = exports_repo::list_recipients(&mut tx, export_id).await?;
    tx.commit().await?;
    Ok((row, recipients))
}

async fn has_export_read_grant(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    export: &exports_repo::MarketingExportRow,
) -> Result<bool, AppError> {
    if !ctx.scopes.contains(Scope::MailRead) {
        return Ok(false);
    }
    let Some(launch) = campaigns_repo::get_launch(tx, export.launch_id).await? else {
        return Ok(false);
    };
    let Some(revision) = campaigns_repo::get_revision(tx, launch.revision_id).await? else {
        return Ok(false);
    };
    let grant = crate::authz::mailbox_grant(tx, ctx, revision.mail_account_id)
        .await?
        .unwrap_or_default();
    Ok(grant.can_read)
}

pub async fn list_exports(
    db: &Db,
    ctx: &Ctx,
    limit: u32,
) -> Result<Vec<MarketingExportResponse>, AppError> {
    require_admin(ctx)?;
    if !(1..=100).contains(&limit) {
        return Err(AppError::validation(
            "limit must be between 1 and 100",
            vec![],
        ));
    }
    let mut tx = db.begin(ctx.workspace_id).await?;
    let rows = exports_repo::list_exports(&mut tx, i64::from(limit)).await?;
    let mut responses = Vec::with_capacity(rows.len());
    for row in rows {
        if !has_export_read_grant(&mut tx, ctx, &row).await? {
            continue;
        }
        let recipients = exports_repo::list_recipients(&mut tx, row.id).await?;
        responses.push(response(row, &recipients)?);
    }
    tx.commit().await?;
    Ok(responses)
}

pub async fn get_export(
    db: &Db,
    ctx: &Ctx,
    export_id: Uuid,
) -> Result<MarketingExportResponse, AppError> {
    require_admin(ctx)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let row = exports_repo::get_export(&mut tx, export_id)
        .await?
        .ok_or_else(|| AppError::not_found("marketing export not found"))?;
    if !has_export_read_grant(&mut tx, ctx, &row).await? {
        return Err(AppError::not_found("marketing export not found"));
    }
    let recipients = exports_repo::list_recipients(&mut tx, export_id).await?;
    tx.commit().await?;
    response(row, &recipients)
}

pub async fn get_content(
    db: &Db,
    ctx: &Ctx,
    export_id: Uuid,
) -> Result<MarketingExportContentResponse, AppError> {
    require_admin(ctx)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let row = exports_repo::get_export(&mut tx, export_id)
        .await?
        .ok_or_else(|| AppError::not_found("marketing export not found"))?;
    if !has_export_read_grant(&mut tx, ctx, &row).await? {
        return Err(AppError::not_found("marketing export not found"));
    }
    tx.commit().await?;
    Ok(MarketingExportContentResponse {
        export_id: row.id,
        subject: row.subject,
        html: row.body_html,
        text: row.body_text,
        export_hash: hex_bytes(&row.export_hash, "export")?,
    })
}

/// Prepare the sole export snapshot for a frozen, one-step Office launch.
/// Provider reads validate fields/topics before the short snapshot transaction;
/// no Zoho write is made here.
pub async fn prepare_export(
    db: &Db,
    ctx: &Ctx,
    config: &Config,
    oauth: Option<&CampaignsHttp>,
    operations: Option<&CampaignsOperations>,
    request: &PrepareMarketingExportRequest,
) -> Result<MarketingExportResponse, AppError> {
    require_admin(ctx)?;
    let operations = operations.ok_or_else(|| {
        AppError::dependency_unavailable("Zoho Campaigns operations are not configured")
    })?;

    let (selected_connection_id, existing) = {
        let mut tx = db.begin(ctx.workspace_id).await?;
        let launch = campaigns_repo::get_launch(&mut tx, request.launch_id)
            .await?
            .ok_or_else(|| AppError::not_found("campaign launch not found"))?;
        let revision = campaigns_repo::get_revision(&mut tx, launch.revision_id)
            .await?
            .ok_or_else(|| AppError::not_found("campaign revision not found"))?;
        if launch.delivery_channel_id != revision.delivery_channel_id {
            return Err(AppError::conflict(
                "launch channel no longer matches the frozen revision",
            ));
        }
        crate::authz::require_mailbox(
            &mut tx,
            ctx,
            revision.mail_account_id,
            crate::authz::MailboxPerm::Compose,
        )
        .await?;
        let channel = campaigns_repo::get_channel(&mut tx, launch.delivery_channel_id)
            .await?
            .ok_or_else(|| AppError::not_found("delivery channel not found"))?;
        if channel.provider != "zoho_campaigns" || channel.purpose != "marketing" {
            return Err(AppError::validation(
                "this export flow requires an Office Zoho Campaigns marketing channel",
                vec![],
            ));
        }
        let connection_id = marketing_repo::channel_connection_id(&mut tx, channel.id)
            .await?
            .ok_or_else(|| AppError::conflict("delivery channel has no Campaigns connection"))?;
        campaigns_repo::get_sender_identity(&mut tx, channel.sender_identity_id)
            .await?
            .ok_or_else(|| AppError::conflict("delivery channel sender is unavailable"))?;
        if marketing_repo::sender_verification_status(&mut tx, channel.sender_identity_id)
            .await?
            .as_deref()
            != Some("verified")
        {
            return Err(AppError::conflict(
                "Campaigns sender identity is not verified",
            ));
        }
        let existing = exports_repo::get_export_by_launch(&mut tx, launch.id).await?;
        tx.commit().await?;
        (connection_id, existing)
    };

    if let Some(existing) = existing {
        if existing.connection_id == selected_connection_id
            && existing.field_map == serde_json::to_value(&request.field_map).unwrap_or(Value::Null)
            && existing.topic_id == request.topic_id
        {
            let (_, recipients) = first_export(db, ctx, existing.id).await?;
            return response(existing, &recipients);
        }
        return Err(AppError::conflict(
            "this launch already has an immutable export with different mappings",
        ));
    }

    let fields_for_prepare = [
        "ZohoCampaigns.campaign.READ",
        "ZohoCampaigns.contact.READ",
        "ZohoCampaigns.contact.CREATE",
        "ZohoCampaigns.contact.UPDATE",
        "ZohoCampaigns.campaign.CREATE",
    ];
    account_and_scopes(db, ctx, selected_connection_id, &fields_for_prepare).await?;
    let tokens = provider_access(db, ctx, config, oauth, selected_connection_id).await?;
    let contact_fields = operations
        .get_contact_fields(&tokens.access_token)
        .await
        .map_err(campaigns_error)?;
    let allowed_fields: BTreeSet<String> = contact_fields
        .into_iter()
        .map(|field| field.display_name)
        .collect();
    let topics = operations
        .get_topics(&tokens.access_token, 1, 100)
        .await
        .map_err(campaigns_error)?;
    if !topics.is_empty() && request.topic_id.is_none() {
        return Err(AppError::validation(
            "select a Campaigns topic for this account",
            vec![],
        ));
    }
    if let Some(topic_id) = request.topic_id.as_deref()
        && !topics.iter().any(|topic| topic.id == topic_id)
    {
        return Err(AppError::validation(
            "selected Campaigns topic is not available to this connection",
            vec![],
        ));
    }
    let mappings = request
        .field_map
        .iter()
        .map(|(variable, mapping)| {
            (
                variable.clone(),
                CampaignFieldMapping {
                    contact_field: mapping.contact_field.clone(),
                    merge_tag: mapping.merge_tag.clone(),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();

    let mut tx = db.begin(ctx.workspace_id).await?;
    let launch = campaigns_repo::get_launch_for_update(&mut tx, request.launch_id)
        .await?
        .ok_or_else(|| AppError::not_found("campaign launch not found"))?;
    super::revalidate_admin_session_tx(&mut tx, ctx).await?;
    if !matches!(
        launch.status.as_str(),
        "frozen" | "awaiting_approval" | "approved"
    ) || launch.expires_at <= Utc::now()
    {
        return Err(AppError::conflict(
            "campaign launch is not an active frozen launch",
        ));
    }
    let Some(source_manifest_hash) = launch.manifest_hash.clone() else {
        return Err(AppError::conflict("campaign launch has no frozen manifest"));
    };
    if source_manifest_hash.len() != 32 {
        return Err(AppError::internal(
            "campaign launch manifest hash is invalid",
        ));
    }
    let revision = campaigns_repo::get_revision(&mut tx, launch.revision_id)
        .await?
        .ok_or_else(|| AppError::internal("campaign revision is missing"))?;
    crate::authz::require_mailbox(
        &mut tx,
        ctx,
        revision.mail_account_id,
        crate::authz::MailboxPerm::Compose,
    )
    .await?;
    validate_immediate_schedule(&revision.schedule_rules)?;
    let steps = campaigns_repo::list_steps(&mut tx, revision.id).await?;
    if steps.len() != 1 || steps[0].step_order != 1 || steps[0].delay_hours != 0 {
        return Err(AppError::validation(
            "Zoho Campaigns Free accepts one immediate Office launch step",
            vec![],
        ));
    }
    let step = &steps[0];
    let channel = campaigns_repo::get_channel(&mut tx, launch.delivery_channel_id)
        .await?
        .ok_or_else(|| AppError::internal("campaign delivery channel is missing"))?;
    if channel.provider != "zoho_campaigns" || channel.purpose != "marketing" {
        return Err(AppError::conflict(
            "launch no longer uses a Campaigns marketing channel",
        ));
    }
    let current_connection_id = marketing_repo::channel_connection_id(&mut tx, channel.id)
        .await?
        .ok_or_else(|| AppError::conflict("Campaigns channel connection is missing"))?;
    if current_connection_id != selected_connection_id {
        return Err(AppError::conflict("Campaigns channel connection changed"));
    }
    if marketing_repo::sender_verification_status(&mut tx, channel.sender_identity_id)
        .await?
        .as_deref()
        != Some("verified")
    {
        return Err(AppError::conflict(
            "Campaigns sender identity is not verified",
        ));
    }
    let (sender_address, sender_name) =
        campaigns_repo::get_sender_identity(&mut tx, channel.sender_identity_id)
            .await?
            .ok_or_else(|| AppError::conflict("Campaigns sender identity is missing"))?;
    let template = campaigns_repo::get_template_version(&mut tx, step.template_version_id)
        .await?
        .ok_or_else(|| AppError::internal("frozen Office template version is missing"))?;
    let enrollments = campaigns_repo::list_enrollments(&mut tx, launch.id).await?;
    let enrollment_addresses: HashMap<Uuid, Uuid> = enrollments
        .into_iter()
        .map(|enrollment| (enrollment.id, enrollment.email_address_id))
        .collect();
    let mut all_outbounds = Vec::new();
    let mut after = None;
    loop {
        let page = campaigns_repo::list_launch_outbounds(&mut tx, launch.id, after, 500).await?;
        if page.is_empty() {
            break;
        }
        after = page
            .last()
            .map(|entry| (entry.outbound.created_at, entry.outbound.id));
        all_outbounds.extend(page.into_iter().map(|entry| entry.outbound));
        if all_outbounds.len() > MAX_EXPORT_RECIPIENTS {
            return Err(AppError::validation(
                "Zoho Campaigns export exceeds the local 2,000 contact cap",
                vec![],
            ));
        }
        if all_outbounds.len() % 500 != 0 {
            break;
        }
    }
    all_outbounds.retain(|outbound| outbound.step_id == step.id);
    if all_outbounds.is_empty() {
        return Err(AppError::validation(
            "campaign launch has no eligible recipients",
            vec![],
        ));
    }
    if all_outbounds
        .iter()
        .any(|outbound| outbound.status != "draft" || outbound.not_before.is_some())
    {
        return Err(AppError::conflict(
            "Campaigns export only accepts unscheduled, frozen Office drafts",
        ));
    }
    let launch_values = exports_repo::list_launch_values(&mut tx, launch.id).await?;
    let values_by_address: HashMap<Uuid, Value> = launch_values
        .into_iter()
        .map(|row| (row.email_address_id, row.variables))
        .collect();
    let mut source_recipients = Vec::with_capacity(all_outbounds.len());
    let mut seen_addresses = HashSet::new();
    let mut seen_normalized = HashSet::new();
    for outbound in all_outbounds {
        let email_address_id = *enrollment_addresses
            .get(&outbound.enrollment_id)
            .ok_or_else(|| {
                AppError::conflict("frozen launch outbound is not linked to this launch")
            })?;
        if !seen_addresses.insert(email_address_id) {
            return Err(AppError::conflict(
                "frozen launch contains a duplicate recipient address",
            ));
        }
        let Some((_, verification)) =
            campaigns_repo::get_email_address(&mut tx, email_address_id).await?
        else {
            return Err(AppError::conflict(
                "frozen recipient address is unavailable",
            ));
        };
        if !recipient_address_status_allowed(&verification) {
            return Err(AppError::new(
                ErrorCode::SendBasisUnverified,
                "Campaigns export recipient address has a blocked verification status",
            ));
        }
        compliance_core::lock_email_address(&mut tx, email_address_id).await?;
        if !compliance_core::active_suppressions(&mut tx, email_address_id, "marketing")
            .await?
            .is_empty()
        {
            return Err(AppError::new(
                ErrorCode::RecipientSuppressed,
                "Campaigns export recipient now has an active suppression",
            ));
        }
        if !has_current_marketing_opt_in(&mut tx, email_address_id).await? {
            return Err(AppError::new(
                ErrorCode::SendBasisUnverified,
                "Campaigns export recipient requires current explicit marketing consent",
            ));
        }
        let normalized = marketing_repo::normalized_email(&mut tx, email_address_id)
            .await?
            .ok_or_else(|| AppError::conflict("frozen recipient email is unavailable"))?;
        if !seen_normalized.insert(normalized.to_ascii_lowercase()) {
            return Err(AppError::conflict(
                "frozen launch contains a duplicate normalized email address",
            ));
        }
        let variables = values_by_address
            .get(&email_address_id)
            .cloned()
            .ok_or_else(|| AppError::conflict("frozen recipient variables are missing"))?;
        source_recipients.push(SourceRecipient {
            email_address_id,
            outbound_message_id: outbound.id,
            normalized_address: normalized,
            variables: parse_variables(variables)?,
        });
    }
    source_recipients.sort_by(|left, right| {
        (&left.normalized_address, left.email_address_id)
            .cmp(&(&right.normalized_address, right.email_address_id))
    });
    let variable_rows: Vec<RecipientVariables> = source_recipients
        .iter()
        .map(|recipient| recipient.variables.clone())
        .collect();
    let content = transform_campaign_content(&CampaignContentInput {
        subject: &template.subject,
        text: &template.body_text,
        html: template.body_html.as_deref(),
        recipients: &variable_rows,
        mappings: &mappings,
        allowed_contact_fields: &allowed_fields,
    })
    .map_err(|error| AppError::validation(error.to_string(), vec![]))?;
    let body_html = content.html.ok_or_else(|| {
        AppError::validation("Campaigns export requires Office HTML content", vec![])
    })?;
    let content_hash = decode_hex_hash(&content.content_hash)
        .ok_or_else(|| AppError::internal("could not encode transformed content hash"))?;
    let field_map = serde_json::to_value(&request.field_map)
        .map_err(|_| AppError::internal("could not encode Campaigns field mappings"))?;
    let audience_fingerprint = serde_json::to_vec(
        &source_recipients
            .iter()
            .zip(content.recipient_fields.iter())
            .map(|(recipient, fields)| {
                (
                    recipient.email_address_id,
                    recipient.outbound_message_id,
                    &recipient.normalized_address,
                    fields,
                )
            })
            .collect::<Vec<_>>(),
    )
    .map_err(|_| AppError::internal("could not fingerprint frozen recipient audience"))?;
    let audience_hash = crypto::hmac_sha256(&config.hmac_key, &audience_fingerprint);
    let request_hash = crypto::hmac_sha256(
        &config.hmac_key,
        &serde_json::to_vec(&json!({
            "launch_id": launch.id,
            "source_manifest": export_hash(&source_manifest_hash),
            "content_hash": export_hash(&content_hash),
            "audience_hash": export_hash(&audience_hash),
            "field_map": &field_map,
            "topic_id": request.topic_id.as_deref(),
        }))
        .map_err(|_| AppError::internal("could not fingerprint Campaigns export snapshot"))?,
    );
    let expires_at = std::cmp::min(launch.expires_at, Utc::now() + EXPORT_TTL);
    if expires_at <= Utc::now() {
        return Err(AppError::conflict("campaign launch has expired"));
    }
    let new_export = exports_repo::NewMarketingExport {
        launch_id: launch.id,
        revision_id: revision.id,
        step_id: step.id,
        delivery_channel_id: channel.id,
        connection_id: selected_connection_id,
        source_manifest_hash,
        content_hash: content_hash.to_vec(),
        audience_hash: audience_hash.to_vec(),
        export_hash: request_hash.to_vec(),
        subject: content.subject,
        body_text: content.text,
        body_html,
        sender_address,
        sender_name,
        field_map,
        prepared_by: ctx.principal_id,
        expires_at,
    };
    let created = exports_repo::insert_export(&mut tx, new_export).await?;
    let recipients = source_recipients
        .into_iter()
        .zip(content.recipient_fields)
        .map(|(recipient, fields)| exports_repo::NewExportRecipient {
            email_address_id: recipient.email_address_id,
            outbound_message_id: recipient.outbound_message_id,
            normalized_address: recipient.normalized_address,
            variables: Value::Object(
                fields
                    .into_iter()
                    .map(|(key, value)| (key, Value::String(value)))
                    .collect(),
            ),
            sync_status: "pending".into(),
        })
        .collect::<Vec<_>>();
    exports_repo::replace_recipients(&mut tx, created.id, &recipients).await?;
    exports_repo::update_execution_metadata(
        &mut tx,
        created.id,
        exports_repo::ExecutionMetadata {
            topic_id: request.topic_id.clone(),
            recipient_count: Some(i64::try_from(recipients.len()).map_err(|_| {
                AppError::internal("marketing export recipient count is out of range")
            })?),
            ..Default::default()
        },
    )
    .await?;
    let mut entry = AuditEntry::new(
        ctx.principal_id,
        "marketing.export_prepared",
        "marketing_export",
        ctx.request_id.clone(),
    )
    .with_target(created.id);
    entry.metadata = json!({
        "launch_id": created.launch_id,
        "recipient_count": recipients.len(),
        "source_manifest_hash": export_hash(&created.source_manifest_hash),
        "export_hash": export_hash(&created.export_hash),
    });
    audit::record(&mut tx, entry).await?;
    let stored = exports_repo::get_export(&mut tx, created.id)
        .await?
        .ok_or_else(|| AppError::internal("prepared marketing export disappeared"))?;
    let stored_recipients = exports_repo::list_recipients(&mut tx, created.id).await?;
    tx.commit().await?;
    response(stored, &stored_recipients)
}

fn parse_variables(value: Value) -> Result<RecipientVariables, AppError> {
    let object = value
        .as_object()
        .ok_or_else(|| AppError::conflict("frozen launch variables are invalid"))?;
    object
        .iter()
        .map(|(key, value)| {
            value
                .as_str()
                .map(|value| (key.clone(), value.to_string()))
                .ok_or_else(|| AppError::conflict("frozen launch variables must be strings"))
        })
        .collect()
}

async fn has_current_marketing_opt_in(
    tx: &mut Tx<'_>,
    email_address_id: Uuid,
) -> Result<bool, AppError> {
    let Some(basis_id) = campaigns_repo::valid_marketing_basis(tx, email_address_id).await? else {
        return Ok(false);
    };
    let Some(basis) = compliance_repo::get_basis(tx, basis_id).await? else {
        return Ok(false);
    };
    Ok(is_explicit_marketing_opt_in(
        &basis.purpose,
        &basis.status,
        &basis.basis_type,
    ))
}

fn is_explicit_marketing_opt_in(purpose: &str, status: &str, basis_type: &str) -> bool {
    purpose == "marketing" && status == "valid" && basis_type == "explicit_consent"
}

async fn set_state(
    db: &Db,
    ctx: &Ctx,
    export_id: Uuid,
    state: ExportState,
    report: Option<Value>,
) -> Result<MarketingExportResponse, AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    let current = exports_repo::get_export_for_update(&mut tx, export_id)
        .await?
        .ok_or_else(|| AppError::not_found("marketing export not found"))?;
    if current.state != state.as_str() {
        exports_repo::update_state(&mut tx, export_id, current.version, state).await?;
    }
    if let Some(report) = report {
        exports_repo::update_execution_metadata(
            &mut tx,
            export_id,
            exports_repo::ExecutionMetadata {
                report: Some(report),
                last_checked_at: Some(Utc::now()),
                ..Default::default()
            },
        )
        .await?;
    }
    let row = exports_repo::get_export(&mut tx, export_id)
        .await?
        .ok_or_else(|| AppError::internal("marketing export disappeared"))?;
    let recipients = exports_repo::list_recipients(&mut tx, export_id).await?;
    tx.commit().await?;
    response(row, &recipients)
}

async fn claim_operation(
    db: &Db,
    ctx: &Ctx,
    config: &Config,
    export_id: Uuid,
    kind: &str,
    correlation: &str,
    request: &impl serde::Serialize,
) -> Result<OperationClaim, AppError> {
    let fingerprint = hmac_request(config, request)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    super::revalidate_admin_session_tx(&mut tx, ctx).await?;
    if kind != "send_campaign" {
        let export = exports_repo::get_export_for_update(&mut tx, export_id)
            .await?
            .ok_or_else(|| AppError::not_found("marketing export not found"))?;
        if export.state != ExportState::Syncing.as_str() {
            return Err(AppError::conflict(
                "marketing export is no longer accepting Campaigns contact writes",
            ));
        }
        verify_connection_scopes_in_tx(
            &mut tx,
            export.connection_id,
            &[
                "ZohoCampaigns.campaign.READ",
                "ZohoCampaigns.campaign.CREATE",
                "ZohoCampaigns.contact.READ",
                "ZohoCampaigns.contact.CREATE",
                "ZohoCampaigns.contact.UPDATE",
            ],
        )
        .await?;
        let launch = campaigns_repo::get_launch(&mut tx, export.launch_id)
            .await?
            .ok_or_else(|| AppError::not_found("campaign launch not found"))?;
        let revision = campaigns_repo::get_revision(&mut tx, launch.revision_id)
            .await?
            .ok_or_else(|| AppError::internal("campaign revision is missing"))?;
        crate::authz::require_mailbox(
            &mut tx,
            ctx,
            revision.mail_account_id,
            crate::authz::MailboxPerm::Compose,
        )
        .await?;
    }
    let claim =
        exports_repo::claim_operation(&mut tx, export_id, kind, correlation, &fingerprint).await?;
    tx.commit().await?;
    Ok(claim)
}

async fn complete_operation(
    db: &Db,
    ctx: &Ctx,
    export_id: Uuid,
    kind: &str,
    correlation: &str,
    provider_id: Option<&str>,
) -> Result<(), AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    exports_repo::complete_operation(&mut tx, export_id, kind, correlation, provider_id).await?;
    tx.commit().await
}

async fn mark_operation_unknown(
    db: &Db,
    ctx: &Ctx,
    export_id: Uuid,
    kind: &str,
    correlation: &str,
    code: &str,
) -> Result<(), AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    exports_repo::mark_operation_unknown(&mut tx, export_id, kind, correlation, Some(code)).await?;
    tx.commit().await
}

async fn mark_operation_rejected(
    db: &Db,
    ctx: &Ctx,
    export_id: Uuid,
    kind: &str,
    correlation: &str,
    code: &str,
) -> Result<(), AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    exports_repo::mark_operation_rejected(&mut tx, export_id, kind, correlation, code).await?;
    tx.commit().await
}

#[allow(clippy::too_many_arguments)] // Carries the immutable operation identity plus verified provider evidence.
async fn resolve_operation_from_readback(
    db: &Db,
    ctx: &Ctx,
    export_id: Uuid,
    kind: &str,
    correlation: &str,
    provider_id: Option<&str>,
    resolution: OperationResolution,
    provider_code: Option<&str>,
) -> Result<(), AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    exports_repo::reconcile_operation(
        &mut tx,
        export_id,
        kind,
        correlation,
        provider_id,
        resolution,
        provider_code,
    )
    .await?;
    tx.commit().await
}

async fn reconcile_or_complete_readback(
    db: &Db,
    ctx: &Ctx,
    export_id: Uuid,
    operation: &exports_repo::MarketingOperationRow,
    provider_id: Option<&str>,
) -> Result<(), AppError> {
    match operation.status.as_str() {
        "unknown" => {
            resolve_operation_from_readback(
                db,
                ctx,
                export_id,
                &operation.kind,
                &operation.correlation_key,
                provider_id,
                OperationResolution::Completed,
                None,
            )
            .await
        }
        "started" => {
            complete_operation(
                db,
                ctx,
                export_id,
                &operation.kind,
                &operation.correlation_key,
                provider_id,
            )
            .await
        }
        _ => Ok(()),
    }
}

fn normalized_campaign_status(status: Option<&str>) -> String {
    status
        .unwrap_or_default()
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn campaign_draft_is_sendable(status: Option<&str>) -> bool {
    normalized_campaign_status(status) == "draft"
}

enum ClaimedOperation {
    New,
    Completed(Option<String>),
}

fn claim_status(claim: OperationClaim) -> Result<ClaimedOperation, AppError> {
    match claim {
        OperationClaim::New => Ok(ClaimedOperation::New),
        OperationClaim::Completed { provider_id } => Ok(ClaimedOperation::Completed(provider_id)),
        OperationClaim::Unknown => Err(AppError::new(
            ErrorCode::ProviderSubmissionOutcomeUnknown,
            "Campaigns write outcome is unknown; refresh to reconcile before continuing",
        )),
        OperationClaim::InProgress => Err(AppError::new(
            ErrorCode::RequestInProgress,
            "Campaigns operation is already in progress",
        )),
        OperationClaim::Rejected { error_code } => Err(AppError::conflict(format!(
            "Campaigns operation was previously rejected ({})",
            error_code
                .as_deref()
                .map(safe_provider_code)
                .unwrap_or_else(|| "UNKNOWN".into())
        ))),
    }
}

async fn mark_export_unknown(
    db: &Db,
    ctx: &Ctx,
    export_id: Uuid,
    report: Value,
) -> Result<MarketingExportResponse, AppError> {
    set_state(db, ctx, export_id, ExportState::Unknown, Some(report)).await
}

async fn mark_export_needs_review(
    db: &Db,
    ctx: &Ctx,
    export_id: Uuid,
    report: Value,
) -> Result<MarketingExportResponse, AppError> {
    set_state(db, ctx, export_id, ExportState::NeedsReview, Some(report)).await
}

async fn mark_export_rejected(
    db: &Db,
    ctx: &Ctx,
    export_id: Uuid,
    report: Value,
) -> Result<MarketingExportResponse, AppError> {
    set_state(db, ctx, export_id, ExportState::Rejected, Some(report)).await
}

async fn update_recipient_states(
    db: &Db,
    ctx: &Ctx,
    export_id: Uuid,
    recipients: &[exports_repo::MarketingExportRecipientRow],
    active: &HashSet<String>,
    unsubscribed: &HashSet<String>,
    bounced: &HashSet<String>,
) -> Result<(), AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    for recipient in recipients {
        let address = recipient.normalized_address.to_ascii_lowercase();
        let state = if unsubscribed.contains(&address) {
            "unsubscribed"
        } else if bounced.contains(&address) {
            "bounced"
        } else if active.contains(&address) {
            "active"
        } else {
            "pending_confirmation"
        };
        exports_repo::set_recipient_status(&mut tx, export_id, recipient.email_address_id, state)
            .await?;
    }
    tx.commit().await
}

async fn sync_progress(
    db: &Db,
    ctx: &Ctx,
    export_id: Uuid,
    writes_this_call: usize,
) -> Result<MarketingExportResponse, AppError> {
    let (_, recipients) = first_export(db, ctx, export_id).await?;
    let accepted = recipients
        .iter()
        .filter(|recipient| {
            matches!(
                recipient.sync_status.as_str(),
                "pending_confirmation" | "active"
            )
        })
        .count();
    report_response(
        db,
        ctx,
        export_id,
        json!({
            "readiness": "syncing",
            "contact_writes_this_call": writes_this_call,
            "contact_sync_requests_accepted": accepted,
            "audience_count": recipients.len(),
            "body_readback": "unavailable",
            "body_readback_note": BODY_READBACK_NOTE,
        }),
        Some(ExportState::Syncing),
    )
    .await
}

async fn read_list_audience(
    operations: &CampaignsOperations,
    access: &infrastructure::providers::zoho_campaigns::SecretValue,
    list_key: &str,
) -> Result<(HashSet<String>, HashSet<String>, HashSet<String>), AppError> {
    let active = operations
        .get_all_list_contacts(access, list_key, CampaignContactStatus::Active)
        .await
        .map_err(campaigns_error)?
        .into_iter()
        .map(|contact| contact.email.trim().to_ascii_lowercase())
        .collect();
    let unsubscribed = operations
        .get_all_list_contacts(access, list_key, CampaignContactStatus::Unsubscribed)
        .await
        .map_err(campaigns_error)?
        .into_iter()
        .map(|contact| contact.email.trim().to_ascii_lowercase())
        .collect();
    let bounced = operations
        .get_all_list_contacts(access, list_key, CampaignContactStatus::Bounced)
        .await
        .map_err(campaigns_error)?
        .into_iter()
        .map(|contact| contact.email.trim().to_ascii_lowercase())
        .collect();
    Ok((active, unsubscribed, bounced))
}

async fn mark_export_unsubscribe_checked(
    db: &Db,
    ctx: &Ctx,
    export_id: Uuid,
) -> Result<(), AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    marketing_repo::mark_unsubscribe_checked(&mut tx, export_id).await?;
    tx.commit().await
}

fn contact_fields_match(observed: &BTreeMap<String, Value>, expected: &Value) -> bool {
    let Some(expected) = expected.as_object() else {
        return false;
    };
    expected.iter().all(|(field, value)| {
        let Some(expected_value) = value.as_str() else {
            return false;
        };
        observed
            .iter()
            .find(|(observed_field, _)| observed_field.eq_ignore_ascii_case(field))
            .and_then(|(_, observed_value)| observed_value.as_str())
            == Some(expected_value)
    })
}

async fn active_contact_fields_match(
    operations: &CampaignsOperations,
    access: &infrastructure::providers::zoho_campaigns::SecretValue,
    list_key: &str,
    recipients: &[exports_repo::MarketingExportRecipientRow],
) -> Result<bool, AppError> {
    let contacts = operations
        .get_all_list_contacts(access, list_key, CampaignContactStatus::Active)
        .await
        .map_err(campaigns_error)?;
    let observed: HashMap<String, BTreeMap<String, Value>> = contacts
        .into_iter()
        .map(|contact| (contact.email.trim().to_ascii_lowercase(), contact.fields))
        .collect();
    Ok(recipients.iter().all(|recipient| {
        observed
            .get(&recipient.normalized_address.to_ascii_lowercase())
            .is_some_and(|fields| contact_fields_match(fields, &recipient.variables))
    }))
}

fn detail_string<'a>(details: &'a BTreeMap<String, Value>, field: &str) -> Option<&'a str> {
    details
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(field))
        .and_then(|(_, value)| value.as_str())
}

fn campaign_metadata_matches(
    export: &exports_repo::MarketingExportRow,
    campaign_key: &str,
    list_key: &str,
    details: &infrastructure::providers::zoho_campaigns::operations::CampaignDetails,
) -> bool {
    let name = format!("Office export {}", export.id);
    let targets = details
        .associated_lists
        .iter()
        .filter_map(|item| {
            item.get("listkey")
                .or_else(|| item.get("list_key"))
                .and_then(Value::as_str)
        })
        .collect::<BTreeSet<_>>();
    targets.len() == 1
        && targets.contains(list_key)
        && detail_string(&details.details, "campaign_name") == Some(name.as_str())
        && detail_string(&details.details, "email_subject") == Some(export.subject.as_str())
        && detail_string(&details.details, "email_from") == Some(export.sender_address.as_str())
        && detail_string(&details.details, "sender_name") == Some(export.sender_name.as_str())
        && details
            .details
            .get("campaign_key")
            .or_else(|| details.details.get("campaignkey"))
            .and_then(Value::as_str)
            .is_none_or(|key| key == campaign_key)
}

async fn current_recipient_eligibility(
    db: &Db,
    ctx: &Ctx,
    recipients: &[exports_repo::MarketingExportRecipientRow],
) -> Result<Option<&'static str>, AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    let result = current_recipient_eligibility_in_tx(&mut tx, recipients).await?;
    tx.commit().await?;
    Ok(result)
}

async fn recipients_have_in_flight_delivery(
    db: &Db,
    ctx: &Ctx,
    export_id: Uuid,
    recipients: &[exports_repo::MarketingExportRecipientRow],
) -> Result<bool, AppError> {
    let mut email_ids = recipients
        .iter()
        .map(|recipient| recipient.email_address_id)
        .collect::<Vec<_>>();
    email_ids.sort_unstable();
    email_ids.dedup();
    let mut tx = db.begin(ctx.workspace_id).await?;
    for email_id in &email_ids {
        if !compliance_core::lock_email_address(&mut tx, *email_id).await? {
            return Err(AppError::conflict(
                "export recipient address is no longer available",
            ));
        }
    }
    let busy = marketing_repo::recipient_delivery_in_flight(&mut tx, export_id, &email_ids).await?;
    tx.commit().await?;
    Ok(busy)
}

async fn current_recipient_eligibility_in_tx(
    tx: &mut Tx<'_>,
    recipients: &[exports_repo::MarketingExportRecipientRow],
) -> Result<Option<&'static str>, AppError> {
    let mut ordered = recipients.to_vec();
    ordered.sort_by_key(|recipient| recipient.email_address_id);
    for recipient in &ordered {
        if !compliance_core::lock_email_address(tx, recipient.email_address_id).await? {
            return Err(AppError::conflict(
                "export recipient address is no longer available",
            ));
        }
        let Some(normalized_address) =
            marketing_repo::normalized_email(tx, recipient.email_address_id).await?
        else {
            return Err(AppError::conflict(
                "export recipient address is no longer available",
            ));
        };
        if !normalized_address.eq_ignore_ascii_case(&recipient.normalized_address) {
            return Err(AppError::conflict(
                "export recipient address no longer matches its Office address record",
            ));
        }
        let suppressions =
            compliance_core::active_suppressions(tx, recipient.email_address_id, "marketing")
                .await?;
        if !suppressions.is_empty() {
            return Ok(Some("recipient_suppressed"));
        }
        if !has_current_marketing_opt_in(tx, recipient.email_address_id).await? {
            return Ok(Some("explicit_marketing_consent_missing"));
        }
        if campaigns_repo::get_email_address(tx, recipient.email_address_id)
            .await?
            .is_none_or(|(_, verification)| !recipient_address_status_allowed(&verification))
        {
            return Ok(Some("recipient_address_blocked"));
        }
    }
    Ok(None)
}

struct PriorOptoutScan {
    processed_lists: usize,
    matched_recipients: usize,
    more_stale_lists: bool,
}

/// A bounded prior-list scan is one logical phase. Reuse its checkpoint only
/// while a previous request explicitly reported more lists and the scan is
/// still recent; otherwise every new sync/send phase starts a fresh read of
/// all known prior lists, regardless of aggregate-report freshness.
fn resumable_prior_scan_epoch(
    report: &Value,
    now: DateTime<Utc>,
    epoch_key: &str,
    more_key: &str,
) -> DateTime<Utc> {
    let resumable = report.get(more_key) == Some(&Value::Bool(true));
    let epoch = report
        .get(epoch_key)
        .and_then(Value::as_i64)
        .and_then(DateTime::<Utc>::from_timestamp_millis);
    if resumable
        && let Some(epoch) = epoch
        && epoch <= now
        && now - epoch < PRIOR_SCAN_RESUME_WINDOW
    {
        return epoch;
    }
    now
}

fn prior_scan_epoch(report: &Value, now: DateTime<Utc>) -> DateTime<Utc> {
    resumable_prior_scan_epoch(
        report,
        now,
        PRIOR_SCAN_EPOCH_REPORT_KEY,
        PRIOR_SCAN_MORE_REPORT_KEY,
    )
}

fn send_prior_scan_epoch(report: &Value, now: DateTime<Utc>) -> DateTime<Utc> {
    resumable_prior_scan_epoch(
        report,
        now,
        SEND_PRIOR_SCAN_EPOCH_REPORT_KEY,
        SEND_PRIOR_SCAN_MORE_REPORT_KEY,
    )
}

#[allow(clippy::too_many_arguments)] // The scan binds frozen audience, provider identity, and phase checkpoint.
async fn scan_prior_provider_optouts(
    db: &Db,
    ctx: &Ctx,
    connection_id: Uuid,
    export_id: Uuid,
    recipients: &[exports_repo::MarketingExportRecipientRow],
    operations: &CampaignsOperations,
    access: &infrastructure::providers::zoho_campaigns::SecretValue,
    scan_epoch: DateTime<Utc>,
) -> Result<PriorOptoutScan, AppError> {
    let email_ids: Vec<Uuid> = recipients
        .iter()
        .map(|recipient| recipient.email_address_id)
        .collect();
    let lists = {
        let mut tx = db.begin(ctx.workspace_id).await?;
        let lists = marketing_repo::prior_provider_lists(
            &mut tx,
            connection_id,
            &email_ids,
            export_id,
            scan_epoch,
            101,
        )
        .await?;
        tx.commit().await?;
        lists
    };
    let more_stale_lists = lists.len() > 100;
    let current_by_address: HashMap<String, Uuid> = recipients
        .iter()
        .map(|recipient| {
            (
                recipient.normalized_address.to_ascii_lowercase(),
                recipient.email_address_id,
            )
        })
        .collect();
    let mut matched_recipients = HashSet::new();
    let lists_to_scan = lists.into_iter().take(100).collect::<Vec<_>>();
    for prior_list in &lists_to_scan {
        let unsubscribers = operations
            .get_all_list_contacts(
                access,
                &prior_list.provider_list_key,
                CampaignContactStatus::Unsubscribed,
            )
            .await
            .map_err(campaigns_error)?;
        let matching_ids = unsubscribers
            .iter()
            .filter_map(|contact| {
                current_by_address
                    .get(&contact.email.trim().to_ascii_lowercase())
                    .copied()
            })
            .collect::<BTreeSet<_>>();

        let mut tx = db.begin(ctx.workspace_id).await?;
        for email_address_id in &matching_ids {
            if !compliance_core::lock_email_address(&mut tx, *email_address_id).await? {
                return Err(AppError::conflict(
                    "prior Campaigns opt-out address is unavailable",
                ));
            }
            let existing =
                compliance_core::active_suppressions(&mut tx, *email_address_id, "marketing")
                    .await?;
            if !existing
                .iter()
                .any(|suppression| suppression.reason == "unsubscribe")
            {
                compliance_core::insert_suppression(
                    &mut tx,
                    *email_address_id,
                    "marketing",
                    "unsubscribe",
                    "zoho_campaigns_prior_list",
                    Some(ctx.principal_id),
                    None,
                )
                .await?;
            }
            exports_repo::set_recipient_status(
                &mut tx,
                export_id,
                *email_address_id,
                "unsubscribed",
            )
            .await?;
            exports_repo::set_recipient_status(
                &mut tx,
                prior_list.export_id,
                *email_address_id,
                "unsubscribed",
            )
            .await?;
            matched_recipients.insert(*email_address_id);
        }
        exports_repo::update_execution_metadata(
            &mut tx,
            prior_list.export_id,
            exports_repo::ExecutionMetadata {
                last_checked_at: Some(Utc::now()),
                ..Default::default()
            },
        )
        .await?;
        marketing_repo::mark_unsubscribe_checked(&mut tx, prior_list.export_id).await?;
        tx.commit().await?;
    }
    Ok(PriorOptoutScan {
        processed_lists: lists_to_scan.len(),
        matched_recipients: matched_recipients.len(),
        more_stale_lists,
    })
}

fn content_url(
    config: &Config,
    workspace_id: Uuid,
    export: &exports_repo::MarketingExportRow,
    base_url: &str,
) -> Result<String, AppError> {
    let origin = infrastructure::config::validate_campaign_content_origin(
        base_url,
        config.app_env.is_dev_or_test(),
    )?;
    let expires = export.expires_at.timestamp();
    if expires <= Utc::now().timestamp() {
        return Err(AppError::conflict(
            "marketing export content URL has expired",
        ));
    }
    let hash = hex_bytes(&export.content_hash, "content")?;
    let signature = content_signature(&config.hmac_key, workspace_id, export.id, expires, &hash);
    Ok(format!(
        "{}{CAMPAIGNS_CONTENT_ROUTE}/{}/{}?expires={}&content_hash={}&signature={}",
        origin, workspace_id, export.id, expires, hash, signature,
    ))
}

fn content_signature(
    key: &[u8],
    workspace_id: Uuid,
    export_id: Uuid,
    expires: i64,
    content_hash: &str,
) -> String {
    let message =
        format!("marketing-content-v1\n{workspace_id}\n{export_id}\n{expires}\n{content_hash}");
    crypto::b64_encode(&crypto::hmac_sha256(key, message.as_bytes()))
}

/// Called by the unauthenticated public route. Invalid capabilities are
/// indistinguishable from missing exports; signatures bind workspace, export,
/// expiry, and the immutable content hash.
pub async fn get_public_content(
    db: &Db,
    config: &Config,
    workspace_id: Uuid,
    export_id: Uuid,
    expires: i64,
    content_hash: &str,
    signature: &str,
) -> Result<String, AppError> {
    let Some(provided_bytes) = crypto::b64_decode(signature) else {
        return Err(AppError::not_found("marketing content not found"));
    };
    let signed_message =
        format!("marketing-content-v1\n{workspace_id}\n{export_id}\n{expires}\n{content_hash}");
    if expires <= Utc::now().timestamp()
        || expires > (Utc::now() + EXPORT_TTL).timestamp()
        || !crypto::hmac_sha256_verify(&config.hmac_key, signed_message.as_bytes(), &provided_bytes)
    {
        return Err(AppError::not_found("marketing content not found"));
    }
    let Some(expected_hash) = decode_hex_hash(content_hash) else {
        return Err(AppError::not_found("marketing content not found"));
    };
    let mut tx = db.begin(workspace_id).await?;
    let row = exports_repo::get_export(&mut tx, export_id)
        .await?
        .ok_or_else(|| AppError::not_found("marketing content not found"))?;
    if row.workspace_id != workspace_id
        || row.content_hash.as_slice() != expected_hash.as_slice()
        || row.expires_at.timestamp() < expires
        || row.expires_at <= Utc::now()
        || row.body_html.chars().count() > 300_000
    {
        return Err(AppError::not_found("marketing content not found"));
    }
    let html = row.body_html;
    tx.commit().await?;
    Ok(html)
}

async fn source_launch_approval(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    export: &exports_repo::MarketingExportRow,
) -> Result<(), AppError> {
    let launch = campaigns_repo::get_launch(tx, export.launch_id)
        .await?
        .ok_or_else(|| AppError::not_found("campaign launch not found"))?;
    if !matches!(launch.status.as_str(), "approved" | "running")
        || launch.expires_at <= Utc::now()
        || launch.manifest_hash.as_deref() != Some(export.source_manifest_hash.as_slice())
    {
        return Err(AppError::conflict(
            "approved Office launch no longer matches this export snapshot",
        ));
    }
    let active = approvals_repo::active_request_for_launch(tx, launch.id)
        .await?
        .ok_or_else(|| AppError::conflict("a live approval for this Office launch is required"))?;
    // Lock the native approval before the send transaction locks the export.
    // Revocation follows the same lock order and cannot race the intent commit.
    let approval = approvals_repo::get_request_for_update(tx, active.id)
        .await?
        .ok_or_else(|| AppError::conflict("the Office launch approval no longer exists"))?;
    if approval.action_type != "campaign_launch"
        || approval.status != "approved"
        || approval.expires_at <= Utc::now()
        || approval.snapshot_hash != export.source_manifest_hash
    {
        return Err(AppError::conflict(
            "a live approval for this Office launch is required",
        ));
    }
    let decision = approvals_repo::get_decision(tx, approval.id)
        .await?
        .filter(|decision| {
            decision.decision == domain::approvals::decision::APPROVED
                && decision.decided_by != approval.requested_by
                && decision.version_at_decision + 1 == approval.version
        })
        .ok_or_else(|| AppError::conflict("the Office launch approval is not current"))?;
    let review = exports_repo::get_export_review(tx, export.id)
        .await?
        .filter(|review| {
            review.approval_request_id == approval.id
                && review.approver_id == decision.decided_by
                && review.export_hash == export.export_hash
        })
        .ok_or_else(|| {
            AppError::conflict("the exact Campaigns export has not received its independent review")
        })?;
    let reviewer = infrastructure::repo::auth::get_principal(tx, review.approver_id)
        .await?
        .ok_or_else(|| AppError::conflict("the Campaigns export reviewer is unavailable"))?;
    if reviewer.kind != "human"
        || reviewer.status != "active"
        || !matches!(reviewer.role.as_str(), "admin" | "manager")
    {
        return Err(AppError::conflict(
            "the Campaigns export reviewer is no longer an active administrator or manager",
        ));
    }
    let revision = campaigns_repo::get_revision(tx, launch.revision_id)
        .await?
        .ok_or_else(|| AppError::internal("campaign revision is missing"))?;
    let mut reviewer_ctx = ctx.clone();
    reviewer_ctx.principal_id = reviewer.id;
    reviewer_ctx.principal_kind = reviewer.principal_kind();
    reviewer_ctx.role = reviewer.role();
    reviewer_ctx.scopes = reviewer.role().implied_scopes();
    reviewer_ctx.permission_version = reviewer.permission_version;
    let reviewer_grant = crate::authz::mailbox_grant(tx, &reviewer_ctx, revision.mail_account_id)
        .await?
        .unwrap_or_default();
    if !reviewer_grant.can_approve {
        return Err(AppError::conflict(
            "the Campaigns export reviewer no longer has mailbox approval access",
        ));
    }
    Ok(())
}

async fn require_channel_for_send(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    export: &exports_repo::MarketingExportRow,
) -> Result<(), AppError> {
    super::revalidate_admin_session_tx(tx, ctx).await?;
    verify_connection_scopes_in_tx(
        tx,
        export.connection_id,
        &[
            "ZohoCampaigns.campaign.READ",
            "ZohoCampaigns.contact.READ",
            "ZohoCampaigns.campaign.UPDATE",
        ],
    )
    .await?;
    let launch = campaigns_repo::get_launch(tx, export.launch_id)
        .await?
        .ok_or_else(|| AppError::not_found("campaign launch not found"))?;
    let revision = campaigns_repo::get_revision(tx, launch.revision_id)
        .await?
        .ok_or_else(|| AppError::internal("campaign revision is missing"))?;
    crate::authz::require_mailbox(
        tx,
        ctx,
        revision.mail_account_id,
        crate::authz::MailboxPerm::Send,
    )
    .await?;
    if campaigns_repo::emergency_stop_active(tx).await? {
        return Err(AppError::new(
            ErrorCode::WorkspaceEmergencyStop,
            "workspace is in emergency stop",
        ));
    }
    let channel = campaigns_repo::get_channel(tx, export.delivery_channel_id)
        .await?
        .ok_or_else(|| AppError::not_found("delivery channel not found"))?;
    if !channel.enabled || channel.policy_status != "allowed" {
        return Err(if channel.policy_status == "blocked" {
            AppError::new(
                ErrorCode::ProviderPolicyBlocked,
                "Campaigns channel is blocked",
            )
        } else {
            AppError::new(
                ErrorCode::ProviderPolicyUnverified,
                "Campaigns channel is not allowed",
            )
        });
    }
    if marketing_repo::channel_connection_id(tx, channel.id).await? != Some(export.connection_id) {
        return Err(AppError::conflict(
            "Campaigns channel connection changed after export",
        ));
    }
    if marketing_repo::sender_verification_status(tx, channel.sender_identity_id)
        .await?
        .as_deref()
        != Some("verified")
    {
        return Err(AppError::conflict(
            "Campaigns sender identity is not verified",
        ));
    }
    source_launch_approval(tx, ctx, export).await
}

async fn report_response(
    db: &Db,
    ctx: &Ctx,
    export_id: Uuid,
    report: Value,
    next_state: Option<ExportState>,
) -> Result<MarketingExportResponse, AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    let current = exports_repo::get_export_for_update(&mut tx, export_id)
        .await?
        .ok_or_else(|| AppError::not_found("marketing export not found"))?;
    if let Some(next) = next_state {
        let transitioned = current.state != next.as_str();
        if transitioned {
            if current.state == ExportState::Ready.as_str()
                && matches!(next, ExportState::InProgress | ExportState::Completed)
            {
                exports_repo::update_state(
                    &mut tx,
                    export_id,
                    current.version,
                    ExportState::Submitting,
                )
                .await?;
                let submitting = exports_repo::get_export_for_update(&mut tx, export_id)
                    .await?
                    .ok_or_else(|| AppError::internal("marketing export disappeared"))?;
                exports_repo::update_state(&mut tx, export_id, submitting.version, next).await?;
            } else {
                exports_repo::update_state(&mut tx, export_id, current.version, next).await?;
            }
        }
        advance_source_launch_for_bulk_tx(&mut tx, ctx, current.launch_id, export_id, next).await?;
        if transitioned && next == ExportState::Ready {
            enqueue_export_poll_in_tx(&mut tx, export_id, EXPORT_POLL_INTERVAL).await?;
        }
    }
    exports_repo::update_execution_metadata(
        &mut tx,
        export_id,
        exports_repo::ExecutionMetadata {
            last_checked_at: Some(Utc::now()),
            report: Some(report),
            preserve_send_scan: next_state.is_none()
                && current.state == ExportState::Ready.as_str(),
            ..Default::default()
        },
    )
    .await?;
    let updated = exports_repo::get_export(&mut tx, export_id)
        .await?
        .ok_or_else(|| AppError::internal("marketing export disappeared"))?;
    let recipients = exports_repo::list_recipients(&mut tx, export_id).await?;
    tx.commit().await?;
    response(updated, &recipients)
}

/// Keep the Office launch lifecycle aligned with the provider-bound bulk
/// operation. The Campaigns branch never creates per-recipient send jobs.
pub(crate) async fn advance_source_launch_for_bulk_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    launch_id: Uuid,
    export_id: Uuid,
    export_state: ExportState,
) -> Result<(), AppError> {
    let target = match export_state {
        ExportState::InProgress => Some(domain::campaigns::launch_status::RUNNING),
        ExportState::Completed => Some(domain::campaigns::launch_status::COMPLETED),
        _ => None,
    };
    let Some(target) = target else { return Ok(()) };
    let Some(mut launch) = campaigns_repo::get_launch_for_update(tx, launch_id).await? else {
        return Ok(());
    };
    if export_state == ExportState::Completed
        && launch.status == domain::campaigns::launch_status::APPROVED
        && domain::campaigns::launch_status::can_transition(
            &launch.status,
            domain::campaigns::launch_status::RUNNING,
        )
    {
        campaigns_repo::transition_launch(
            tx,
            launch_id,
            domain::campaigns::launch_status::RUNNING,
            Some(launch.version),
        )
        .await?;
        launch = campaigns_repo::get_launch_for_update(tx, launch_id)
            .await?
            .ok_or_else(|| AppError::internal("campaign launch disappeared"))?;
        record_bulk_launch_transition(tx, ctx, &launch, export_id, "running").await?;
    }
    if launch.status != target
        && domain::campaigns::launch_status::can_transition(&launch.status, target)
    {
        campaigns_repo::transition_launch(tx, launch_id, target, Some(launch.version)).await?;
        launch = campaigns_repo::get_launch_for_update(tx, launch_id)
            .await?
            .ok_or_else(|| AppError::internal("campaign launch disappeared"))?;
        record_bulk_launch_transition(tx, ctx, &launch, export_id, target).await?;
    }
    Ok(())
}

async fn record_bulk_launch_transition(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    launch: &campaigns_repo::LaunchRow,
    export_id: Uuid,
    state: &str,
) -> Result<(), AppError> {
    let completed = state == domain::campaigns::launch_status::COMPLETED;
    let action = if completed {
        "campaign.launch_bulk_completed"
    } else {
        "campaign.launch_bulk_activated"
    };
    let event = if completed {
        "campaign.launch_completed"
    } else {
        "campaign.launch_activated"
    };
    let mut entry = AuditEntry::new(
        ctx.principal_id,
        action,
        "campaign_launch",
        ctx.request_id.clone(),
    )
    .with_target(launch.id);
    entry.metadata =
        json!({"delivery_mode":"zoho_campaigns_bulk","export_id":export_id,"state":state});
    audit::record(tx, entry).await?;
    outbox::emit(
        tx,
        event,
        "campaign_launch",
        launch.id,
        launch.version,
        json!({"launch_id":launch.id,"export_id":export_id,"delivery_mode":"zoho_campaigns_bulk","status":state}),
    )
    .await?;
    Ok(())
}

pub async fn sync_export(
    db: &Db,
    ctx: &Ctx,
    config: &Config,
    oauth: Option<&CampaignsHttp>,
    operations: Option<&CampaignsOperations>,
    export_id: Uuid,
) -> Result<MarketingExportResponse, AppError> {
    require_admin(ctx)?;
    let operations = operations.ok_or_else(|| {
        AppError::dependency_unavailable("Campaigns operations are not configured")
    })?;
    let (mut export, recipients) = first_export(db, ctx, export_id).await?;
    let _connection_guard =
        super::lock_connection_operation(db, ctx.workspace_id, export.connection_id).await?;
    if matches!(
        export.state.as_str(),
        "ready" | "unknown" | "rejected" | "canceled" | "in_progress" | "completed" | "submitting"
    ) {
        return response(export, &recipients);
    }
    if !matches!(
        export.state.as_str(),
        "preparing" | "syncing" | "needs_review"
    ) {
        return Err(AppError::conflict(
            "marketing export is not ready for contact sync",
        ));
    }
    {
        let mut tx = db.begin(ctx.workspace_id).await?;
        let launch = campaigns_repo::get_launch(&mut tx, export.launch_id)
            .await?
            .ok_or_else(|| AppError::not_found("campaign launch not found"))?;
        let revision = campaigns_repo::get_revision(&mut tx, launch.revision_id)
            .await?
            .ok_or_else(|| AppError::internal("campaign revision is missing"))?;
        crate::authz::require_mailbox(
            &mut tx,
            ctx,
            revision.mail_account_id,
            crate::authz::MailboxPerm::Compose,
        )
        .await?;
        tx.commit().await?;
    }
    let required = [
        "ZohoCampaigns.campaign.READ",
        "ZohoCampaigns.contact.READ",
        "ZohoCampaigns.contact.CREATE",
        "ZohoCampaigns.contact.UPDATE",
        "ZohoCampaigns.campaign.CREATE",
    ];
    account_and_scopes(db, ctx, export.connection_id, &required).await?;
    let tokens = provider_access(db, ctx, config, oauth, export.connection_id).await?;

    let expected: HashSet<String> = recipients
        .iter()
        .map(|recipient| recipient.normalized_address.to_ascii_lowercase())
        .collect();
    if expected.len() != recipients.len() || expected.is_empty() {
        return mark_export_needs_review(
            db,
            ctx,
            export_id,
            json!({
                "readiness": "needs_review",
                "reason": "audience_not_unique_or_empty",
            }),
        )
        .await;
    }
    let scan_epoch = prior_scan_epoch(&export.report, Utc::now());
    let prior_optouts = scan_prior_provider_optouts(
        db,
        ctx,
        export.connection_id,
        export_id,
        &recipients,
        operations,
        &tokens.access_token,
        scan_epoch,
    )
    .await?;
    if prior_optouts.more_stale_lists {
        return report_response(
            db,
            ctx,
            export_id,
            json!({
                "readiness":"syncing",
                "reason":"prior_provider_opt_out_scan_continues",
                "prior_lists_checked":prior_optouts.processed_lists,
                "contact_writes_this_call":0,
                "audience_count":recipients.len(),
                PRIOR_SCAN_EPOCH_REPORT_KEY:scan_epoch.timestamp_millis(),
                PRIOR_SCAN_MORE_REPORT_KEY:true,
            }),
            Some(ExportState::Syncing),
        )
        .await;
    }
    if prior_optouts.matched_recipients > 0 {
        return mark_export_needs_review(
            db,
            ctx,
            export_id,
            json!({
                "readiness":"needs_review",
                "reason":"provider_opt_out_mirrored_to_office",
                "excluded_count":prior_optouts.matched_recipients,
                "prior_lists_checked":prior_optouts.processed_lists,
            }),
        )
        .await;
    }
    if let Some(reason) = current_recipient_eligibility(db, ctx, &recipients).await? {
        return mark_export_needs_review(
            db,
            ctx,
            export_id,
            json!({"readiness":"needs_review","reason":reason}),
        )
        .await;
    }
    if recipients_have_in_flight_delivery(db, ctx, export_id, &recipients).await? {
        return Err(AppError::conflict(
            "an overlapping recipient is still in another Campaigns delivery",
        ));
    }
    if export.state != ExportState::Syncing.as_str() {
        set_state(db, ctx, export_id, ExportState::Syncing, None).await?;
        export.state = ExportState::Syncing.as_str().to_string();
    }
    let mut writes_this_call = 0usize;
    let mut list_key = export.provider_list_key.clone();
    if list_key.is_none() {
        let correlation = "create_list";
        let first_batch: Vec<String> = recipients
            .iter()
            .take(10)
            .map(|recipient| recipient.normalized_address.clone())
            .collect();
        let fingerprint = json!({
            "list_name": format!("Office export {}", export.id),
            "emails": first_batch,
            "signup_form": "private",
        });
        if let Some(reason) = current_recipient_eligibility(
            db,
            ctx,
            &recipients.iter().take(10).cloned().collect::<Vec<_>>(),
        )
        .await?
        {
            return mark_export_needs_review(
                db,
                ctx,
                export_id,
                json!({"readiness":"needs_review","reason":reason}),
            )
            .await;
        }
        let claim = claim_operation(
            db,
            ctx,
            config,
            export_id,
            "create_list",
            correlation,
            &fingerprint,
        )
        .await?;
        match claim_status(claim)? {
            ClaimedOperation::Completed(Some(provider_id)) => list_key = Some(provider_id),
            ClaimedOperation::Completed(None) => {
                return Err(AppError::conflict(
                    "completed Campaigns list creation has no provider key",
                ));
            }
            ClaimedOperation::New => {
                if writes_this_call == MAX_CONTACT_WRITES_PER_SYNC {
                    return sync_progress(db, ctx, export_id, writes_this_call).await;
                }
                writes_this_call += 1;
                let created = operations
                    .create_list_with_contacts(
                        &tokens.access_token,
                        CreateMailingListRequest {
                            list_name: format!("Office export {}", export.id),
                            email_addresses: recipients
                                .iter()
                                .take(10)
                                .map(|recipient| recipient.normalized_address.clone())
                                .collect(),
                            signup_form: CampaignSignupForm::Private,
                            description: Some(format!("Office export {}", export.id)),
                        },
                    )
                    .await;
                let created = match created {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        if matches!(
                            error,
                            CampaignsOperationsError::Transport
                                | CampaignsOperationsError::HttpStatus { .. }
                                | CampaignsOperationsError::InvalidResponse
                        ) {
                            mark_operation_unknown(
                                db,
                                ctx,
                                export_id,
                                "create_list",
                                correlation,
                                "transport_unknown",
                            )
                            .await?;
                            return mark_export_unknown(
                                db,
                                ctx,
                                export_id,
                                json!({"readiness":"unknown","operation":"create_list"}),
                            )
                            .await;
                        }
                        mark_operation_rejected(
                            db,
                            ctx,
                            export_id,
                            "create_list",
                            correlation,
                            "provider_read_failed",
                        )
                        .await?;
                        return Err(campaigns_error(error));
                    }
                };
                match created {
                    CampaignMutationOutcome::Accepted(created) => {
                        list_key = Some(created.list_key.clone());
                        complete_operation(
                            db,
                            ctx,
                            export_id,
                            "create_list",
                            correlation,
                            Some(&created.list_key),
                        )
                        .await?;
                        let mut tx = db.begin(ctx.workspace_id).await?;
                        exports_repo::update_execution_metadata(
                            &mut tx,
                            export_id,
                            exports_repo::ExecutionMetadata {
                                provider_list_key: Some(created.list_key.clone()),
                                topic_id: export.topic_id.clone(),
                                ..Default::default()
                            },
                        )
                        .await?;
                        tx.commit().await?;
                    }
                    CampaignMutationOutcome::Rejected { code } => {
                        mark_operation_rejected(
                            db,
                            ctx,
                            export_id,
                            "create_list",
                            correlation,
                            &safe_provider_code(&code),
                        )
                        .await?;
                        return mark_export_rejected(db, ctx, export_id, json!({"readiness":"rejected","operation":"create_list","provider_code":safe_provider_code(&code)})).await;
                    }
                    CampaignMutationOutcome::Unknown => {
                        mark_operation_unknown(
                            db,
                            ctx,
                            export_id,
                            "create_list",
                            correlation,
                            "unknown",
                        )
                        .await?;
                        return mark_export_unknown(
                            db,
                            ctx,
                            export_id,
                            json!({"readiness":"unknown","operation":"create_list"}),
                        )
                        .await;
                    }
                }
            }
        }
    }
    let list_key = list_key.ok_or_else(|| AppError::internal("Campaigns list key is missing"))?;
    export.provider_list_key = Some(list_key.clone());

    for (batch_index, batch) in recipients
        .iter()
        .skip(10)
        .collect::<Vec<_>>()
        .chunks(10)
        .enumerate()
    {
        let correlation = format!("batch_{batch_index}");
        let emails: Vec<String> = batch
            .iter()
            .map(|recipient| recipient.normalized_address.clone())
            .collect();
        let fingerprint = json!({"list_key":list_key,"emails":emails});
        let batch_recipients: Vec<_> = batch.iter().map(|recipient| (*recipient).clone()).collect();
        if let Some(reason) = current_recipient_eligibility(db, ctx, &batch_recipients).await? {
            return mark_export_needs_review(
                db,
                ctx,
                export_id,
                json!({"readiness":"needs_review","reason":reason}),
            )
            .await;
        }
        match claim_status(
            claim_operation(
                db,
                ctx,
                config,
                export_id,
                "add_contacts",
                &correlation,
                &fingerprint,
            )
            .await?,
        )? {
            ClaimedOperation::Completed(_) => continue,
            ClaimedOperation::New => {
                if writes_this_call == MAX_CONTACT_WRITES_PER_SYNC {
                    return sync_progress(db, ctx, export_id, writes_this_call).await;
                }
                writes_this_call += 1;
            }
        }
        match operations
            .add_contacts_to_existing_list(&tokens.access_token, &list_key, &emails)
            .await
        {
            Ok(CampaignMutationOutcome::Accepted(())) => {
                complete_operation(db, ctx, export_id, "add_contacts", &correlation, None).await?
            }
            Ok(CampaignMutationOutcome::Rejected { code }) => {
                mark_operation_rejected(
                    db,
                    ctx,
                    export_id,
                    "add_contacts",
                    &correlation,
                    &safe_provider_code(&code),
                )
                .await?;
                return mark_export_rejected(db, ctx, export_id, json!({"readiness":"rejected","operation":"add_contacts","provider_code":safe_provider_code(&code)})).await;
            }
            Ok(CampaignMutationOutcome::Unknown) | Err(CampaignsOperationsError::Transport) => {
                mark_operation_unknown(db, ctx, export_id, "add_contacts", &correlation, "unknown")
                    .await?;
                return mark_export_unknown(
                    db,
                    ctx,
                    export_id,
                    json!({"readiness":"unknown","operation":"add_contacts"}),
                )
                .await;
            }
            Err(error) => {
                mark_operation_rejected(
                    db,
                    ctx,
                    export_id,
                    "add_contacts",
                    &correlation,
                    "provider_error",
                )
                .await?;
                return Err(campaigns_error(error));
            }
        }
        if writes_this_call == MAX_CONTACT_WRITES_PER_SYNC {
            return sync_progress(db, ctx, export_id, writes_this_call).await;
        }
    }

    for recipient in &recipients {
        let fields: BTreeMap<String, String> = serde_json::from_value(recipient.variables.clone())
            .map_err(|_| AppError::conflict("frozen Campaigns contact fields are invalid"))?;
        let contact = ContactInput {
            email: recipient.normalized_address.clone(),
            fields: fields.clone(),
        };
        let correlation = format!("recipient_{}", recipient.email_address_id);
        let fingerprint = json!({"list_key":list_key,"email":contact.email,"fields":fields,"topic_id":export.topic_id});
        if let Some(reason) =
            current_recipient_eligibility(db, ctx, std::slice::from_ref(recipient)).await?
        {
            return mark_export_needs_review(
                db,
                ctx,
                export_id,
                json!({"readiness":"needs_review","reason":reason}),
            )
            .await;
        }
        match claim_status(
            claim_operation(
                db,
                ctx,
                config,
                export_id,
                "subscribe",
                &correlation,
                &fingerprint,
            )
            .await?,
        )? {
            ClaimedOperation::Completed(_) => continue,
            ClaimedOperation::New => {
                if writes_this_call == MAX_CONTACT_WRITES_PER_SYNC {
                    return sync_progress(db, ctx, export_id, writes_this_call).await;
                }
                writes_this_call += 1;
            }
        }
        match operations
            .subscribe_to_list(
                &tokens.access_token,
                &list_key,
                contact,
                export.topic_id.as_deref(),
            )
            .await
        {
            Ok(CampaignMutationOutcome::Accepted(_)) => {
                complete_operation(db, ctx, export_id, "subscribe", &correlation, None).await?;
                let mut tx = db.begin(ctx.workspace_id).await?;
                exports_repo::set_recipient_status(
                    &mut tx,
                    export_id,
                    recipient.email_address_id,
                    "pending_confirmation",
                )
                .await?;
                tx.commit().await?;
            }
            Ok(CampaignMutationOutcome::Rejected { code }) => {
                mark_operation_rejected(
                    db,
                    ctx,
                    export_id,
                    "subscribe",
                    &correlation,
                    &safe_provider_code(&code),
                )
                .await?;
                return mark_export_needs_review(db, ctx, export_id, json!({"readiness":"needs_review","reason":"provider_rejected_contact","provider_code":safe_provider_code(&code)})).await;
            }
            Ok(CampaignMutationOutcome::Unknown) | Err(CampaignsOperationsError::Transport) => {
                mark_operation_unknown(db, ctx, export_id, "subscribe", &correlation, "unknown")
                    .await?;
                return mark_export_unknown(
                    db,
                    ctx,
                    export_id,
                    json!({"readiness":"unknown","operation":"subscribe"}),
                )
                .await;
            }
            Err(error) => {
                mark_operation_rejected(
                    db,
                    ctx,
                    export_id,
                    "subscribe",
                    &correlation,
                    "provider_error",
                )
                .await?;
                return Err(campaigns_error(error));
            }
        }
        if writes_this_call == MAX_CONTACT_WRITES_PER_SYNC {
            return sync_progress(db, ctx, export_id, writes_this_call).await;
        }
    }
    let (active, unsubscribed, bounced) =
        read_list_audience(operations, &tokens.access_token, &list_key).await?;
    mark_export_unsubscribe_checked(db, ctx, export_id).await?;
    update_recipient_states(
        db,
        ctx,
        export_id,
        &recipients,
        &active,
        &unsubscribed,
        &bounced,
    )
    .await?;
    if active != expected || !unsubscribed.is_empty() || !bounced.is_empty() {
        return mark_export_needs_review(
            db,
            ctx,
            export_id,
            json!({
                "readiness": "needs_review",
                "reason": "provider_list_not_exactly_active",
                "expected_count": expected.len(),
                "active_count": active.intersection(&expected).count(),
                "unsubscribed_count": unsubscribed.len(),
                "bounced_count": bounced.len(),
            }),
        )
        .await;
    }
    if !active_contact_fields_match(operations, &tokens.access_token, &list_key, &recipients)
        .await?
    {
        return mark_export_needs_review(
            db,
            ctx,
            export_id,
            json!({
                "readiness": "needs_review",
                "reason": "provider_contact_fields_do_not_match_office_snapshot",
                "audience_count": expected.len(),
            }),
        )
        .await;
    }

    let current_export = first_export(db, ctx, export_id).await?.0;
    let origin = {
        let mut tx = db.begin(ctx.workspace_id).await?;
        let origin = sending_repo::public_base_url(&mut tx).await?;
        tx.commit().await?;
        origin
    };
    let url = content_url(config, ctx.workspace_id, &current_export, &origin)?;
    let create_correlation = "create_campaign";
    let create_fingerprint = json!({
        "campaign_name": format!("Office export {}", export_id),
        "subject": current_export.subject,
        "from_email": current_export.sender_address,
        "from_name": current_export.sender_name,
        "list_key": list_key,
        "topic_id": current_export.topic_id,
        "content_hash": export_hash(&current_export.content_hash),
    });
    let create_claim = claim_operation(
        db,
        ctx,
        config,
        export_id,
        "create_campaign",
        create_correlation,
        &create_fingerprint,
    )
    .await?;
    let campaign_key = match claim_status(create_claim)? {
        ClaimedOperation::Completed(Some(provider_id)) => provider_id,
        ClaimedOperation::Completed(None) => {
            return Err(AppError::conflict(
                "completed Campaigns draft has no provider key",
            ));
        }
        ClaimedOperation::New => match operations
            .create_campaign(
                &tokens.access_token,
                CampaignCreateRequest {
                    name: format!("Office export {}", export_id),
                    subject: current_export.subject.clone(),
                    from_email: current_export.sender_address.clone(),
                    from_name: current_export.sender_name.clone(),
                    list_key: list_key.clone(),
                    topic_id: current_export.topic_id.clone(),
                    content_url: url,
                },
            )
            .await
        {
            Ok(CampaignMutationOutcome::Accepted(created)) => {
                complete_operation(
                    db,
                    ctx,
                    export_id,
                    "create_campaign",
                    create_correlation,
                    Some(&created.campaign_key),
                )
                .await?;
                let mut tx = db.begin(ctx.workspace_id).await?;
                exports_repo::update_execution_metadata(
                    &mut tx,
                    export_id,
                    exports_repo::ExecutionMetadata {
                        campaign_key: Some(created.campaign_key.clone()),
                        topic_id: current_export.topic_id.clone(),
                        provider_list_key: Some(list_key.clone()),
                        ..Default::default()
                    },
                )
                .await?;
                tx.commit().await?;
                created.campaign_key
            }
            Ok(CampaignMutationOutcome::Rejected { code }) => {
                mark_operation_rejected(
                    db,
                    ctx,
                    export_id,
                    "create_campaign",
                    create_correlation,
                    &safe_provider_code(&code),
                )
                .await?;
                return mark_export_rejected(db, ctx, export_id, json!({"readiness":"rejected","operation":"create_campaign","provider_code":safe_provider_code(&code)})).await;
            }
            Ok(CampaignMutationOutcome::Unknown) | Err(CampaignsOperationsError::Transport) => {
                mark_operation_unknown(
                    db,
                    ctx,
                    export_id,
                    "create_campaign",
                    create_correlation,
                    "unknown",
                )
                .await?;
                return mark_export_unknown(
                    db,
                    ctx,
                    export_id,
                    json!({"readiness":"unknown","operation":"create_campaign"}),
                )
                .await;
            }
            Err(error) => {
                mark_operation_rejected(
                    db,
                    ctx,
                    export_id,
                    "create_campaign",
                    create_correlation,
                    "provider_error",
                )
                .await?;
                return Err(campaigns_error(error));
            }
        },
    };

    let details = operations
        .get_campaign_details(&tokens.access_token, &campaign_key, CampaignType::Normal)
        .await
        .map_err(campaigns_error)?;
    if !campaign_metadata_matches(&current_export, &campaign_key, &list_key, &details)
        || !campaign_draft_is_sendable(details.status.as_deref())
    {
        return mark_export_needs_review(
            db,
            ctx,
            export_id,
            json!({
                "readiness": "needs_review",
                "reason": "provider_campaign_metadata_changed_or_unavailable",
                "campaign_status": details.status,
            }),
        )
        .await;
    }
    let report = json!({
        "body_readback": "unavailable",
        "body_readback_note": BODY_READBACK_NOTE,
        "external_provider_edits": PROVIDER_EDIT_NOTE,
        "provider_metadata_verified": true,
        "provider_list_exact": true,
        "active_count": active.len(),
        "audience_count": expected.len(),
        "campaign_status": details.status,
    });
    report_response(db, ctx, export_id, report, Some(ExportState::Ready)).await
}

fn current_exact_list(
    expected: &HashSet<String>,
    active: &HashSet<String>,
    unsubscribed: &HashSet<String>,
    bounced: &HashSet<String>,
) -> bool {
    expected == active && unsubscribed.is_empty() && bounced.is_empty()
}

#[derive(Clone, Copy)]
enum SendJournalOutcome {
    Completed,
    Rejected,
    Unknown,
}

#[allow(clippy::too_many_arguments)] // One short transaction binds provider outcome, operation, and export state.
async fn store_send_result(
    db: &Db,
    ctx: &Ctx,
    export_id: Uuid,
    correlation: &str,
    provider_id: &str,
    next_state: ExportState,
    terminal: SendJournalOutcome,
    provider_code: Option<&str>,
    report: Value,
) -> Result<MarketingExportResponse, AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    match terminal {
        SendJournalOutcome::Completed => {
            exports_repo::complete_operation(
                &mut tx,
                export_id,
                "send_campaign",
                correlation,
                Some(provider_id),
            )
            .await?;
        }
        SendJournalOutcome::Rejected => {
            exports_repo::mark_operation_rejected(
                &mut tx,
                export_id,
                "send_campaign",
                correlation,
                provider_code.unwrap_or("provider_rejected"),
            )
            .await?;
        }
        SendJournalOutcome::Unknown => {
            exports_repo::mark_operation_unknown(
                &mut tx,
                export_id,
                "send_campaign",
                correlation,
                provider_code,
            )
            .await?;
        }
    }
    let current = exports_repo::get_export_for_update(&mut tx, export_id)
        .await?
        .ok_or_else(|| AppError::not_found("marketing export not found"))?;
    if current.state != next_state.as_str() {
        exports_repo::update_state(&mut tx, export_id, current.version, next_state).await?;
    }
    advance_source_launch_for_bulk_tx(&mut tx, ctx, current.launch_id, export_id, next_state)
        .await?;
    match next_state {
        ExportState::InProgress => {
            enqueue_export_poll_in_tx(&mut tx, export_id, EXPORT_POLL_INTERVAL).await?;
        }
        ExportState::Unknown => {
            enqueue_export_poll_in_tx(&mut tx, export_id, Duration::seconds(30)).await?;
        }
        _ => {}
    }
    exports_repo::update_execution_metadata(
        &mut tx,
        export_id,
        exports_repo::ExecutionMetadata {
            last_checked_at: Some(Utc::now()),
            report: Some(report),
            ..Default::default()
        },
    )
    .await?;
    let row = exports_repo::get_export(&mut tx, export_id)
        .await?
        .ok_or_else(|| AppError::internal("marketing export disappeared"))?;
    let recipients = exports_repo::list_recipients(&mut tx, export_id).await?;
    let mut entry = AuditEntry::new(
        ctx.principal_id,
        "marketing.export_send_result",
        "marketing_export",
        ctx.request_id.clone(),
    )
    .with_target(export_id);
    entry.metadata = json!({
        "state": next_state.as_str(),
        "provider_campaign_key": provider_id,
        "export_hash": export_hash(&row.export_hash),
        "recipient_count": row.recipient_count,
    });
    audit::record(&mut tx, entry).await?;
    tx.commit().await?;
    response(row, &recipients)
}

async fn reserve_send_submission(
    db: &Db,
    ctx: &Ctx,
    export: &exports_repo::MarketingExportRow,
    recipients: &[exports_repo::MarketingExportRecipientRow],
    correlation: &str,
    fingerprint: &[u8; 32],
) -> Result<Option<MarketingExportResponse>, AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    require_channel_for_send(&mut tx, ctx, export).await?;
    let current = exports_repo::get_export_for_update(&mut tx, export.id)
        .await?
        .ok_or_else(|| AppError::not_found("marketing export not found"))?;
    if current.state != ExportState::Ready.as_str() {
        return Err(AppError::conflict(
            "marketing export state changed before send",
        ));
    }
    if let Some(reason) = current_recipient_eligibility_in_tx(&mut tx, recipients).await? {
        exports_repo::update_state(
            &mut tx,
            export.id,
            current.version,
            ExportState::NeedsReview,
        )
        .await?;
        exports_repo::update_execution_metadata(
            &mut tx,
            export.id,
            exports_repo::ExecutionMetadata {
                last_checked_at: Some(Utc::now()),
                report: Some(json!({"readiness":"needs_review","reason":reason})),
                ..Default::default()
            },
        )
        .await?;
        let row = exports_repo::get_export(&mut tx, export.id)
            .await?
            .ok_or_else(|| AppError::internal("marketing export disappeared"))?;
        let current_recipients = exports_repo::list_recipients(&mut tx, export.id).await?;
        tx.commit().await?;
        return Ok(Some(response(row, &current_recipients)?));
    }
    let mut email_ids = recipients
        .iter()
        .map(|recipient| recipient.email_address_id)
        .collect::<Vec<_>>();
    email_ids.sort_unstable();
    email_ids.dedup();
    if marketing_repo::recipient_delivery_in_flight(&mut tx, export.id, &email_ids).await? {
        return Err(AppError::conflict(
            "an overlapping recipient is still in another Campaigns delivery",
        ));
    }

    match exports_repo::claim_operation(
        &mut tx,
        export.id,
        "send_campaign",
        correlation,
        fingerprint,
    )
    .await?
    {
        OperationClaim::New => {
            exports_repo::update_state(
                &mut tx,
                export.id,
                current.version,
                ExportState::Submitting,
            )
            .await?;
            tx.commit().await?;
            Ok(None)
        }
        OperationClaim::Completed { .. } => {
            exports_repo::update_state(
                &mut tx,
                export.id,
                current.version,
                ExportState::Submitting,
            )
            .await?;
            let submitting = exports_repo::get_export_for_update(&mut tx, export.id)
                .await?
                .ok_or_else(|| AppError::internal("marketing export disappeared"))?;
            exports_repo::update_state(
                &mut tx,
                export.id,
                submitting.version,
                ExportState::InProgress,
            )
            .await?;
            advance_source_launch_for_bulk_tx(
                &mut tx,
                ctx,
                current.launch_id,
                export.id,
                ExportState::InProgress,
            )
            .await?;
            enqueue_export_poll_in_tx(&mut tx, export.id, EXPORT_POLL_INTERVAL).await?;
            exports_repo::update_execution_metadata(
                &mut tx,
                export.id,
                exports_repo::ExecutionMetadata {
                    report: Some(
                        json!({"provider_status":"inprogress","reconciled_from_journal":true}),
                    ),
                    last_checked_at: Some(Utc::now()),
                    ..Default::default()
                },
            )
            .await?;
            let row = exports_repo::get_export(&mut tx, export.id)
                .await?
                .ok_or_else(|| AppError::internal("marketing export disappeared"))?;
            let current_recipients = exports_repo::list_recipients(&mut tx, export.id).await?;
            tx.commit().await?;
            Ok(Some(response(row, &current_recipients)?))
        }
        OperationClaim::Unknown => {
            exports_repo::update_state(
                &mut tx,
                export.id,
                current.version,
                ExportState::Submitting,
            )
            .await?;
            let submitting = exports_repo::get_export_for_update(&mut tx, export.id)
                .await?
                .ok_or_else(|| AppError::internal("marketing export disappeared"))?;
            exports_repo::update_state(
                &mut tx,
                export.id,
                submitting.version,
                ExportState::Unknown,
            )
            .await?;
            enqueue_export_poll_in_tx(&mut tx, export.id, Duration::seconds(30)).await?;
            let row = exports_repo::get_export(&mut tx, export.id)
                .await?
                .ok_or_else(|| AppError::internal("marketing export disappeared"))?;
            let current_recipients = exports_repo::list_recipients(&mut tx, export.id).await?;
            tx.commit().await?;
            Ok(Some(response(row, &current_recipients)?))
        }
        OperationClaim::InProgress => Err(AppError::new(
            ErrorCode::RequestInProgress,
            "Campaigns send operation is already in progress",
        )),
        OperationClaim::Rejected { error_code } => {
            exports_repo::update_state(
                &mut tx,
                export.id,
                current.version,
                ExportState::Submitting,
            )
            .await?;
            let submitting = exports_repo::get_export_for_update(&mut tx, export.id)
                .await?
                .ok_or_else(|| AppError::internal("marketing export disappeared"))?;
            exports_repo::update_state(
                &mut tx,
                export.id,
                submitting.version,
                ExportState::Rejected,
            )
            .await?;
            exports_repo::update_execution_metadata(
                &mut tx,
                export.id,
                exports_repo::ExecutionMetadata {
                    report: Some(json!({
                        "readiness":"rejected",
                        "reason":"previous_campaign_send_rejection",
                        "provider_code":error_code.as_deref().map(safe_provider_code).unwrap_or_else(|| "UNKNOWN".into()),
                    })),
                    last_checked_at: Some(Utc::now()),
                    ..Default::default()
                },
            )
            .await?;
            let row = exports_repo::get_export(&mut tx, export.id)
                .await?
                .ok_or_else(|| AppError::internal("marketing export disappeared"))?;
            let current_recipients = exports_repo::list_recipients(&mut tx, export.id).await?;
            tx.commit().await?;
            Ok(Some(response(row, &current_recipients)?))
        }
    }
}

pub async fn send_export(
    db: &Db,
    ctx: &Ctx,
    config: &Config,
    oauth: Option<&CampaignsHttp>,
    operations: Option<&CampaignsOperations>,
    export_id: Uuid,
    request: &SendMarketingExportRequest,
) -> Result<MarketingExportResponse, AppError> {
    require_admin(ctx)?;
    if request.confirmation != "SEND" {
        return Err(AppError::validation("confirmation must be SEND", vec![]));
    }
    let (export, recipients) = first_export(db, ctx, export_id).await?;
    let _connection_guard =
        super::lock_connection_operation(db, ctx.workspace_id, export.connection_id).await?;
    let Some(requested_hash) = decode_hex_hash(&request.expected_export_hash) else {
        return Err(AppError::new(
            ErrorCode::SnapshotMismatch,
            "export hash changed; refresh the Office export before sending",
        ));
    };
    if requested_hash.as_slice() != export.export_hash.as_slice() {
        return Err(AppError::new(
            ErrorCode::SnapshotMismatch,
            "export hash changed; refresh the Office export before sending",
        ));
    }
    if matches!(
        export.state.as_str(),
        "submitting" | "in_progress" | "completed" | "unknown" | "rejected"
    ) {
        return response(export, &recipients);
    }
    if export.state != ExportState::Ready.as_str() {
        return Err(AppError::conflict(
            "only a ready Campaigns export can be sent",
        ));
    }
    if export.expires_at <= Utc::now() {
        return Err(AppError::conflict("marketing export has expired"));
    }
    if let Some(reason) = current_recipient_eligibility(db, ctx, &recipients).await? {
        return mark_export_needs_review(
            db,
            ctx,
            export_id,
            json!({
                "readiness":"needs_review",
                "reason":reason,
            }),
        )
        .await;
    }
    {
        let mut tx = db.begin(ctx.workspace_id).await?;
        require_channel_for_send(&mut tx, ctx, &export).await?;
        tx.commit().await?;
    }
    let required = [
        "ZohoCampaigns.campaign.READ",
        "ZohoCampaigns.contact.READ",
        "ZohoCampaigns.campaign.UPDATE",
    ];
    account_and_scopes(db, ctx, export.connection_id, &required).await?;
    let oauth = oauth
        .ok_or_else(|| AppError::dependency_unavailable("Campaigns OAuth is not configured"))?;
    let tokens = provider_access(db, ctx, config, Some(oauth), export.connection_id).await?;
    let operations = operations.ok_or_else(|| {
        AppError::dependency_unavailable("Campaigns operations are not configured")
    })?;
    let list_key = export
        .provider_list_key
        .as_deref()
        .ok_or_else(|| AppError::conflict("Campaigns list is missing"))?;
    let expected: HashSet<String> = recipients
        .iter()
        .map(|recipient| recipient.normalized_address.to_ascii_lowercase())
        .collect();
    let (active, unsubscribed, bounced) =
        read_list_audience(operations, &tokens.access_token, list_key).await?;
    mark_export_unsubscribe_checked(db, ctx, export_id).await?;
    update_recipient_states(
        db,
        ctx,
        export_id,
        &recipients,
        &active,
        &unsubscribed,
        &bounced,
    )
    .await?;
    if !current_exact_list(&expected, &active, &unsubscribed, &bounced) {
        return mark_export_needs_review(
            db,
            ctx,
            export_id,
            json!({
                "readiness":"needs_review",
                "reason":"provider_list_changed_before_send",
                "expected_count":expected.len(),
                "active_count":active.intersection(&expected).count(),
                "unsubscribed_count":unsubscribed.len(),
                "bounced_count":bounced.len(),
            }),
        )
        .await;
    }
    let campaign_key = export
        .campaign_key
        .as_deref()
        .ok_or_else(|| AppError::conflict("Campaigns draft is missing"))?;
    if !active_contact_fields_match(operations, &tokens.access_token, list_key, &recipients).await?
    {
        return mark_export_needs_review(
            db,
            ctx,
            export_id,
            json!({
                "readiness":"needs_review",
                "reason":"provider_contact_fields_changed_before_send",
            }),
        )
        .await;
    }
    let details = operations
        .get_campaign_details(&tokens.access_token, campaign_key, CampaignType::Normal)
        .await
        .map_err(campaigns_error)?;
    if !campaign_metadata_matches(&export, campaign_key, list_key, &details)
        || !campaign_draft_is_sendable(details.status.as_deref())
    {
        return mark_export_needs_review(
            db,
            ctx,
            export_id,
            json!({
                "readiness":"needs_review",
                "reason":"provider_campaign_metadata_changed_before_send",
            }),
        )
        .await;
    }
    let send_scan_epoch = send_prior_scan_epoch(&export.report, Utc::now());
    let prior_optouts = match scan_prior_provider_optouts(
        db,
        ctx,
        export.connection_id,
        export_id,
        &recipients,
        operations,
        &tokens.access_token,
        send_scan_epoch,
    )
    .await
    {
        Ok(scan) => scan,
        Err(_) => {
            return mark_export_needs_review(
                db,
                ctx,
                export_id,
                json!({
                    "readiness":"needs_review",
                    "reason":"prior_provider_optout_coverage_unavailable",
                    "send_not_submitted":true,
                }),
            )
            .await;
        }
    };
    if prior_optouts.matched_recipients > 0 {
        return mark_export_needs_review(
            db,
            ctx,
            export_id,
            json!({
                "readiness":"needs_review",
                "reason":"provider_opt_out_mirrored_to_office",
                "excluded_count":prior_optouts.matched_recipients,
                "prior_lists_checked":prior_optouts.processed_lists,
                "send_not_submitted":true,
            }),
        )
        .await;
    }
    if prior_optouts.more_stale_lists {
        return report_response(
            db,
            ctx,
            export_id,
            json!({
                "readiness":"ready",
                "reason":"prior_provider_optout_scan_incomplete",
                "send_not_submitted":true,
                "prior_lists_checked":prior_optouts.processed_lists,
                SEND_PRIOR_SCAN_EPOCH_REPORT_KEY:send_scan_epoch.timestamp_millis(),
                SEND_PRIOR_SCAN_MORE_REPORT_KEY:true,
            }),
            None,
        )
        .await;
    }
    // A completed scan cannot be reused after a later pre-submit conflict.
    // Recording `more=false` means the next SEND attempt starts a new fresh
    // phase instead of treating this successful read as permanent evidence.
    report_response(
        db,
        ctx,
        export_id,
        json!({
            "readiness":"ready",
            "reason":"prior_provider_optout_scan_complete",
            "send_not_submitted":true,
            "prior_lists_checked":prior_optouts.processed_lists,
            SEND_PRIOR_SCAN_EPOCH_REPORT_KEY:send_scan_epoch.timestamp_millis(),
            SEND_PRIOR_SCAN_MORE_REPORT_KEY:false,
        }),
        None,
    )
    .await?;
    let correlation = "send_campaign";
    let fingerprint = json!({"campaign_key":campaign_key,"export_hash":request.expected_export_hash,"confirmation":"SEND"});
    let fingerprint = hmac_request(config, &fingerprint)?;
    if let Some(response) =
        reserve_send_submission(db, ctx, &export, &recipients, correlation, &fingerprint).await?
    {
        return Ok(response);
    }

    match operations.send_campaign(&tokens.access_token, campaign_key).await {
        Ok(CampaignMutationOutcome::Accepted(accepted)) if accepted.status == "inprogress" => {
            store_send_result(
                db,
                ctx,
                export_id,
                correlation,
                campaign_key,
                ExportState::InProgress,
                SendJournalOutcome::Completed,
                None,
                json!({
                    "readiness":"in_progress",
                    "provider_status":"inprogress",
                    "body_readback":"unavailable",
                    "body_readback_note":BODY_READBACK_NOTE,
                    "external_provider_edits":PROVIDER_EDIT_NOTE,
                }),
            )
            .await
        }
        Ok(CampaignMutationOutcome::Accepted(accepted)) => {
            // A successful response with an undocumented state still means
            // Zoho may have accepted the bulk submission; never send again.
            store_send_result(
                db,
                ctx,
                export_id,
                correlation,
                campaign_key,
                ExportState::Unknown,
                SendJournalOutcome::Unknown,
                Some("unrecognized_success_status"),
                json!({"readiness":"unknown","operation":"send_campaign","provider_status":safe_provider_code(&accepted.status)}),
            )
            .await
        }
        Ok(CampaignMutationOutcome::Rejected { code }) => {
            store_send_result(
                db,
                ctx,
                export_id,
                correlation,
                campaign_key,
                ExportState::Rejected,
                SendJournalOutcome::Rejected,
                Some(&safe_provider_code(&code)),
                json!({"readiness":"rejected","operation":"send_campaign","provider_code":safe_provider_code(&code)}),
            )
            .await
        }
        Ok(CampaignMutationOutcome::Unknown) | Err(CampaignsOperationsError::Transport) => {
            store_send_result(
                db,
                ctx,
                export_id,
                correlation,
                campaign_key,
                ExportState::Unknown,
                SendJournalOutcome::Unknown,
                Some("unknown"),
                json!({"readiness":"unknown","operation":"send_campaign"}),
            )
            .await
        }
        Err(error) => {
            // The provider call started after the intent commit. Even a
            // non-transport failure is conservatively terminal for sending.
            store_send_result(
                db,
                ctx,
                export_id,
                correlation,
                campaign_key,
                ExportState::Unknown,
                SendJournalOutcome::Unknown,
                Some("provider_error_after_intent"),
                json!({"readiness":"unknown","operation":"send_campaign","provider_code":safe_provider_code(&format!("{error:?}"))}),
            )
            .await
        }
    }
}

pub async fn refresh_export(
    db: &Db,
    ctx: &Ctx,
    config: &Config,
    oauth: Option<&CampaignsHttp>,
    operations: Option<&CampaignsOperations>,
    export_id: Uuid,
) -> Result<MarketingExportResponse, AppError> {
    require_admin(ctx)?;
    let operations = operations.ok_or_else(|| {
        AppError::dependency_unavailable("Campaigns operations are not configured")
    })?;
    let (mut export, recipients) = first_export(db, ctx, export_id).await?;
    let required = ["ZohoCampaigns.campaign.READ", "ZohoCampaigns.contact.READ"];
    account_and_scopes(db, ctx, export.connection_id, &required).await?;
    let oauth = oauth
        .ok_or_else(|| AppError::dependency_unavailable("Campaigns OAuth is not configured"))?;
    let tokens = provider_access(db, ctx, config, Some(oauth), export.connection_id).await?;
    let operation_rows = {
        let mut tx = db.begin(ctx.workspace_id).await?;
        let rows = exports_repo::list_operations(&mut tx, export_id).await?;
        tx.commit().await?;
        rows
    };
    let campaign_key = export.campaign_key.as_deref();
    let list_key = export.provider_list_key.as_deref();
    let mut report = json!({
        "coverage_status": "partial",
        "recipient_events_complete": false,
        "body_readback": "unavailable",
        "body_readback_note": BODY_READBACK_NOTE,
        "external_provider_edits": PROVIDER_EDIT_NOTE,
        "observed_at": Utc::now(),
    });
    let mut next_state = None;
    if let Some(campaign_key) = campaign_key {
        let details = operations
            .get_campaign_details(&tokens.access_token, campaign_key, CampaignType::Normal)
            .await
            .map_err(campaigns_error)?;
        let status = normalized_campaign_status(details.status.as_deref());
        let report_available = matches!(status.as_str(), "sent" | "completed");
        let mut metrics = serde_json::Map::new();
        let mut optout_count = 0usize;
        let mut bounce_count = 0usize;
        let mut local_optouts = Vec::new();
        if report_available {
            if let Ok(aggregate) = operations
                .get_campaign_report(&tokens.access_token, campaign_key)
                .await
            {
                metrics = aggregate
                    .metrics
                    .into_iter()
                    .filter_map(|(key, value)| {
                        let parsed = value
                            .as_f64()
                            .or_else(|| value.as_str().and_then(|text| text.parse::<f64>().ok()))?;
                        serde_json::Number::from_f64(parsed)
                            .map(|number| (key, Value::Number(number)))
                    })
                    .collect();
            } else {
                report["aggregate_report"] = json!("unavailable");
            }
            for action in [
                CampaignRecipientAction::OptOutContacts,
                CampaignRecipientAction::SentHardBounce,
                CampaignRecipientAction::SentSoftBounce,
            ] {
                if let Ok(page) = operations
                    .get_campaign_recipients_data(&tokens.access_token, campaign_key, action)
                    .await
                {
                    let records = page.contacts;
                    if action == CampaignRecipientAction::OptOutContacts {
                        optout_count = records.len();
                        local_optouts.extend(
                            records
                                .into_iter()
                                .map(|record| record.email.trim().to_ascii_lowercase()),
                        );
                    } else {
                        bounce_count += records.len();
                    }
                } else {
                    report["recipient_report"] = json!("partial_or_unavailable");
                }
            }
        }
        report["aggregate_totals"] = Value::Object(metrics);
        report["campaign_status"] = details
            .status
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null);
        report["recipient_events_seen"] = json!({"opt_out":optout_count,"bounce":bounce_count});
        report["coverage_status"] = json!("partial");
        if let Some(send_operation) = operation_rows.iter().find(|operation| {
            operation.kind == "send_campaign"
                && operation.correlation_key == "send_campaign"
                && matches!(
                    operation.status.as_str(),
                    "unknown" | "started" | "completed"
                )
        }) {
            let status = normalized_campaign_status(details.status.as_deref());
            let resolved = match status.as_str() {
                "inprogress" | "sending" => Some(ExportState::InProgress),
                "sent" | "completed" => Some(ExportState::Completed),
                _ => None,
            };
            if let Some(state) = resolved
                && list_key.is_some_and(|list_key| {
                    campaign_metadata_matches(&export, campaign_key, list_key, &details)
                })
            {
                if matches!(send_operation.status.as_str(), "unknown" | "started") {
                    reconcile_or_complete_readback(
                        db,
                        ctx,
                        export_id,
                        send_operation,
                        Some(campaign_key),
                    )
                    .await?;
                }
                next_state = Some(state);
                report["reconciliation"] = json!("send_accepted_by_campaigns_readback");
            } else if resolved.is_some() {
                report["reconciliation"] =
                    json!("campaign_metadata_mismatch; human review required");
            }
        }
        if !local_optouts.is_empty() {
            let stored: HashSet<String> = recipients
                .iter()
                .map(|recipient| recipient.normalized_address.trim().to_ascii_lowercase())
                .collect();
            report["mapped_opt_outs"] = json!(
                local_optouts
                    .iter()
                    .filter(|email| stored.contains(*email))
                    .count()
            );
        }
    } else {
        let expected_campaign_name = format!("Office export {}", export.id);
        if let Some(create_operation) = operation_rows.iter().find(|operation| {
            operation.kind == "create_campaign"
                && operation.correlation_key == "create_campaign"
                && matches!(operation.status.as_str(), "unknown" | "started")
        }) {
            if let Ok(campaigns) = oauth.recent_campaigns(&tokens.access_token).await {
                let matches = campaigns
                    .iter()
                    .filter(|campaign| campaign.name == expected_campaign_name)
                    .collect::<Vec<_>>();
                if matches.len() == 1
                    && let Some(list_key) = export.provider_list_key.as_deref()
                    && let Ok(details) = operations
                        .get_campaign_details(
                            &tokens.access_token,
                            &matches[0].key,
                            CampaignType::Normal,
                        )
                        .await
                    && campaign_metadata_matches(&export, &matches[0].key, list_key, &details)
                    && campaign_draft_is_sendable(details.status.as_deref())
                {
                    reconcile_or_complete_readback(
                        db,
                        ctx,
                        export_id,
                        create_operation,
                        Some(&matches[0].key),
                    )
                    .await?;
                    let mut tx = db.begin(ctx.workspace_id).await?;
                    exports_repo::update_execution_metadata(
                        &mut tx,
                        export_id,
                        exports_repo::ExecutionMetadata {
                            campaign_key: Some(matches[0].key.clone()),
                            ..Default::default()
                        },
                    )
                    .await?;
                    tx.commit().await?;
                    export.campaign_key = Some(matches[0].key.clone());
                    next_state = Some(ExportState::NeedsReview);
                    report["reconciliation"] = json!("campaign_draft_found_by_metadata_readback");
                } else {
                    report["reconciliation"] =
                        json!("campaign_draft_not_proven; human review required");
                }
            } else {
                report["reconciliation"] =
                    json!("campaign_draft_lookup_unavailable; human review required");
            }
        }

        if export.provider_list_key.is_none()
            && let Some(create_operation) = operation_rows.iter().find(|operation| {
                operation.kind == "create_list"
                    && operation.correlation_key == "create_list"
                    && matches!(operation.status.as_str(), "unknown" | "started")
            })
        {
            if let Ok(page) = operations
                .get_mailing_lists(&tokens.access_token, 1, 100, CampaignsSortOrder::Desc)
                .await
            {
                let expected_list_name = format!("Office export {}", export.id);
                let matches = page
                    .entries
                    .iter()
                    .filter(|list| list.name == expected_list_name)
                    .collect::<Vec<_>>();
                if matches.len() == 1 {
                    reconcile_or_complete_readback(
                        db,
                        ctx,
                        export_id,
                        create_operation,
                        Some(&matches[0].list_key),
                    )
                    .await?;
                    let mut tx = db.begin(ctx.workspace_id).await?;
                    exports_repo::update_execution_metadata(
                        &mut tx,
                        export_id,
                        exports_repo::ExecutionMetadata {
                            provider_list_key: Some(matches[0].list_key.clone()),
                            ..Default::default()
                        },
                    )
                    .await?;
                    tx.commit().await?;
                    export.provider_list_key = Some(matches[0].list_key.clone());
                    next_state = Some(ExportState::NeedsReview);
                    report["reconciliation"] = json!("campaign_list_found_by_name_readback");
                } else {
                    report["reconciliation"] =
                        json!("campaign_list_not_proven; human review required");
                }
            } else {
                report["reconciliation"] =
                    json!("campaign_list_lookup_unavailable; human review required");
            }
        }
    }

    if let Some(list_key) = export.provider_list_key.as_deref() {
        let (active, unsubscribed, bounced) =
            read_list_audience(operations, &tokens.access_token, list_key).await?;
        mark_export_unsubscribe_checked(db, ctx, export_id).await?;
        update_recipient_states(
            db,
            ctx,
            export_id,
            &recipients,
            &active,
            &unsubscribed,
            &bounced,
        )
        .await?;
        report["list_status_counts"] = json!({
            "active": active.len(),
            "unsubscribed": unsubscribed.len(),
            "bounced": bounced.len(),
        });
        let audience: HashSet<String> = recipients
            .iter()
            .map(|recipient| recipient.normalized_address.to_ascii_lowercase())
            .collect();
        let audience_matches = current_exact_list(&audience, &active, &unsubscribed, &bounced)
            && active_contact_fields_match(operations, &tokens.access_token, list_key, &recipients)
                .await?;
        let send_operation_pending = operation_rows.iter().any(|operation| {
            operation.kind == "send_campaign"
                && operation.correlation_key == "send_campaign"
                && matches!(operation.status.as_str(), "unknown" | "started")
        });
        if audience_matches && !send_operation_pending {
            let unresolved_contacts = operation_rows.iter().filter(|operation| {
                matches!(operation.kind.as_str(), "add_contacts" | "subscribe")
                    && matches!(operation.status.as_str(), "unknown" | "started")
            });
            let mut resolved_any = false;
            for operation in unresolved_contacts {
                reconcile_or_complete_readback(db, ctx, export_id, operation, None).await?;
                resolved_any = true;
            }
            if resolved_any && export.state == ExportState::Unknown.as_str() {
                next_state = Some(ExportState::NeedsReview);
                report["reconciliation"] = json!("provider_contact_snapshot_matches_office");
            }
        }
    }
    report_response(db, ctx, export_id, report, next_state).await
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_address_status_uses_default_contact_state_without_accepting_known_risks() {
        assert!(super::recipient_address_status_allowed("unknown"));
        assert!(super::recipient_address_status_allowed("verified"));
        for status in ["invalid", "risky", "unexpected", ""] {
            assert!(!super::recipient_address_status_allowed(status));
        }
    }
    use super::*;

    #[test]
    fn signed_content_capability_binds_workspace_export_expiry_and_hash() {
        let key = b"test signing key material";
        let workspace = Uuid::new_v4();
        let export = Uuid::new_v4();
        let hash = "11".repeat(32);
        let expiry = 1_800_000_000;
        let signature = content_signature(key, workspace, export, expiry, &hash);
        let decoded = crypto::b64_decode(&signature).unwrap();
        let message = format!("marketing-content-v1\n{workspace}\n{export}\n{expiry}\n{hash}");
        assert!(crypto::hmac_sha256_verify(
            key,
            message.as_bytes(),
            &decoded
        ));
        let changed = format!(
            "marketing-content-v1\n{workspace}\n{export}\n{}\n{hash}",
            expiry + 1
        );
        assert!(!crypto::hmac_sha256_verify(
            key,
            changed.as_bytes(),
            &decoded
        ));
    }

    #[test]
    fn only_current_explicit_marketing_consent_can_enter_campaigns() {
        assert!(is_explicit_marketing_opt_in(
            "marketing",
            "valid",
            "explicit_consent"
        ));
        assert!(!is_explicit_marketing_opt_in(
            "marketing",
            "valid",
            "reviewed_exception"
        ));
        assert!(!is_explicit_marketing_opt_in(
            "marketing",
            "revoked",
            "explicit_consent"
        ));
        assert!(!is_explicit_marketing_opt_in(
            "correspondence",
            "valid",
            "explicit_consent"
        ));
    }

    #[test]
    fn prior_optout_scan_reuses_only_a_recent_explicitly_incomplete_epoch() {
        let now = DateTime::<Utc>::from_timestamp_millis(1_800_000_000_000).unwrap();
        let recent_epoch = now - Duration::minutes(2);
        let resumable = json!({
            PRIOR_SCAN_EPOCH_REPORT_KEY: recent_epoch.timestamp_millis(),
            PRIOR_SCAN_MORE_REPORT_KEY: true,
        });
        assert_eq!(prior_scan_epoch(&resumable, now), recent_epoch);
        let resumable_send = json!({
            SEND_PRIOR_SCAN_EPOCH_REPORT_KEY: recent_epoch.timestamp_millis(),
            SEND_PRIOR_SCAN_MORE_REPORT_KEY: true,
        });
        assert_eq!(send_prior_scan_epoch(&resumable_send, now), recent_epoch);
        assert_eq!(
            send_prior_scan_epoch(&resumable, now),
            now,
            "sync progress must not be mistaken for an incomplete final SEND scan"
        );

        let complete = json!({
            PRIOR_SCAN_EPOCH_REPORT_KEY: recent_epoch.timestamp_millis(),
            PRIOR_SCAN_MORE_REPORT_KEY: false,
        });
        assert_eq!(prior_scan_epoch(&complete, now), now);

        let expired_epoch = now - Duration::minutes(6);
        let expired = json!({
            PRIOR_SCAN_EPOCH_REPORT_KEY: expired_epoch.timestamp_millis(),
            PRIOR_SCAN_MORE_REPORT_KEY: true,
        });
        assert_eq!(prior_scan_epoch(&expired, now), now);

        // Last report freshness is not an unsubscribe checkpoint. New phases
        // start now even when only aggregate status is present.
        assert_eq!(
            prior_scan_epoch(&json!({"last_checked_at": now.to_rfc3339()}), now),
            now
        );
    }

    #[test]
    fn free_campaigns_accepts_only_immediate_unrestricted_schedules() {
        assert!(validate_immediate_schedule(&json!({})).is_ok());
        assert!(
            validate_immediate_schedule(&json!({
                "weekdays": [1, 2, 3, 4, 5, 6, 7],
                "window_start": "00:00",
                "window_end": "23:59",
                "meeting_type_id": Uuid::new_v4(),
            }))
            .is_ok()
        );
        assert!(validate_immediate_schedule(&json!({"weekdays": [1, 2, 3, 4, 5]})).is_err());
        assert!(
            validate_immediate_schedule(&json!({
                "window_start": "09:00",
                "window_end": "17:00"
            }))
            .is_err()
        );
        assert!(
            validate_immediate_schedule(&json!({
                "launch_not_before": (Utc::now() + Duration::hours(1)).to_rfc3339()
            }))
            .is_err()
        );
    }

    #[test]
    fn campaign_details_must_match_sender_subject_name_and_exact_list() {
        let id = Uuid::new_v4();
        let export = exports_repo::MarketingExportRow {
            id,
            workspace_id: Uuid::new_v4(),
            launch_id: Uuid::new_v4(),
            revision_id: Uuid::new_v4(),
            step_id: Uuid::new_v4(),
            delivery_channel_id: Uuid::new_v4(),
            connection_id: Uuid::new_v4(),
            source_manifest_hash: vec![1; 32],
            content_hash: vec![2; 32],
            audience_hash: vec![3; 32],
            export_hash: vec![4; 32],
            subject: "Subject".into(),
            body_text: "Body".into(),
            body_html: "<p>Body</p>".into(),
            sender_address: "news@example.test".into(),
            sender_name: "Office News".into(),
            field_map: json!({}),
            provider_list_key: Some("list-1".into()),
            topic_id: None,
            campaign_key: Some("campaign-1".into()),
            state: "ready".into(),
            version: 1,
            prepared_by: Uuid::new_v4(),
            created_at: Utc::now(),
            expires_at: Utc::now() + Duration::days(1),
            last_checked_at: None,
            report: json!({}),
            recipient_count: 1,
        };
        let details = infrastructure::providers::zoho_campaigns::operations::CampaignDetails {
            status: Some("Draft".into()),
            details: BTreeMap::from([
                ("campaign_name".into(), json!(format!("Office export {id}"))),
                ("email_subject".into(), json!("Subject")),
                ("email_from".into(), json!("news@example.test")),
                ("sender_name".into(), json!("Office News")),
                ("campaign_key".into(), json!("campaign-1")),
            ]),
            associated_lists: vec![BTreeMap::from([("listkey".into(), json!("list-1"))])],
        };
        assert!(campaign_metadata_matches(
            &export,
            "campaign-1",
            "list-1",
            &details
        ));
        assert!(!campaign_metadata_matches(
            &export,
            "campaign-1",
            "other-list",
            &details
        ));
    }

    #[test]
    fn mapped_provider_fields_must_match_each_frozen_recipient_value() {
        let observed =
            BTreeMap::from([("Booking URL".into(), json!("https://book.example.test/a"))]);
        let expected = json!({"Booking URL":"https://book.example.test/a"});
        assert!(contact_fields_match(&observed, &expected));
        assert!(!contact_fields_match(
            &observed,
            &json!({"Booking URL":"https://book.example.test/b"})
        ));
    }
}
