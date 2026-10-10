//! Background synchronization between Office compliance state and the
//! regular Zoho Campaigns v1.1 suppression registry.
//!
//! Office remains authoritative for opt-outs. Campaigns writes are journaled
//! before I/O, and an unknown result is terminal until an operator resolves
//! it; the provider's Do-Not-Mail write is never automatically resubmitted.

use std::collections::{BTreeSet, HashMap};

use chrono::{Duration, Utc};
use contracts::marketing::MarketingSuppressionSyncResponse;
use contracts::{AppError, AuthMethod, Ctx, ErrorCode, PrincipalKind, Role};
use domain::{
    email_address::{LocalPartPolicy, normalize},
    marketing::{MarketingExportState, OperationClaim},
};
use infrastructure::{
    audit::{self, AuditEntry},
    config::Config,
    db::Db,
    jobs,
    providers::zoho_campaigns::{
        http::CampaignsHttp,
        operations::{
            CampaignContactRecord, CampaignContactStatus, CampaignMutationOutcome,
            CampaignRecipientAction, CampaignType, CampaignsOperations, CampaignsOperationsError,
        },
    },
    repo::{campaigns, compliance_core, marketing, marketing_exports, marketing_suppressions},
};
use serde_json::{Value, json};
use uuid::Uuid;

const CONNECTION_PAGE_SIZE: i64 = 100;
const MAX_CONNECTION_PAGES: usize = 10;
const POLL_INTERVAL: Duration = Duration::minutes(5);
const COMPLETED_POLL_INTERVAL: Duration = Duration::hours(1);
const POLL_KIND: &str = "marketing.poll_export";

/// Return operator-visible Zoho status for a local suppression. Access uses
/// the existing Campaigns connection authorization so these provider results
/// stay limited to human administrators.
pub async fn get_suppression_sync_status(
    db: &Db,
    ctx: &Ctx,
    suppression_id: Uuid,
) -> Result<Vec<MarketingSuppressionSyncResponse>, AppError> {
    super::authorize(db, ctx, "marketing.suppression_sync.read").await?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    if infrastructure::repo::compliance::get_suppression(&mut tx, suppression_id)
        .await?
        .is_none()
    {
        tx.rollback().await?;
        return Err(AppError::not_found("suppression not found"));
    }
    let rows =
        marketing_suppressions::list_suppression_sync_statuses(&mut tx, suppression_id).await?;
    tx.rollback().await?;
    if rows.is_empty() {
        return Ok(vec![MarketingSuppressionSyncResponse {
            connection_id: Uuid::nil(),
            connection_name: "Zoho Campaigns".into(),
            status: "not_connected".into(),
            provider_code: None,
            updated_at: None,
        }]);
    }
    Ok(rows
        .into_iter()
        .map(|row| MarketingSuppressionSyncResponse {
            connection_id: row.connection_id,
            connection_name: row.connection_name,
            status: if row.status == "started" {
                "in_progress".into()
            } else {
                row.status
            },
            provider_code: row.provider_code,
            updated_at: row.updated_at,
        })
        .collect())
}

/// Queue a DB-only suppression reconciliation in the caller's successful
/// Campaigns connection transaction.
pub async fn enqueue_suppression_backfill_in_tx(
    tx: &mut infrastructure::db::Tx<'_>,
    connection_id: Uuid,
) -> Result<(), AppError> {
    marketing_suppressions::enqueue_suppression_backfill_in_tx(tx, connection_id).await
}

fn require_system(ctx: &Ctx) -> Result<(), AppError> {
    if ctx.principal_kind != PrincipalKind::System
        || ctx.role != Role::System
        || !matches!(&ctx.auth, AuthMethod::System { .. })
    {
        return Err(AppError::forbidden(
            "marketing background synchronization requires a system job context",
        ));
    }
    Ok(())
}

fn campaigns_error(error: CampaignsOperationsError) -> AppError {
    match error {
        CampaignsOperationsError::ReauthRequired => AppError::new(
            ErrorCode::IntegrationReauthRequired,
            "Zoho Campaigns authorization must be renewed",
        ),
        CampaignsOperationsError::RateLimited {
            retry_after_seconds,
        } => AppError::new(
            ErrorCode::RateLimited,
            "Zoho Campaigns rate limited the request",
        )
        .with_retry_after(retry_after_seconds.unwrap_or(60)),
        CampaignsOperationsError::Rejected { code } => AppError::conflict(format!(
            "Zoho Campaigns rejected the operation with code {}",
            safe_code(&code)
        )),
        CampaignsOperationsError::InvalidRequest => {
            AppError::validation("Zoho Campaigns request is invalid", vec![])
        }
        CampaignsOperationsError::RequestTooLarge => {
            AppError::validation("Zoho Campaigns request exceeds its limit", vec![])
        }
        CampaignsOperationsError::NotConfigured
        | CampaignsOperationsError::InvalidConfiguration
        | CampaignsOperationsError::Transport
        | CampaignsOperationsError::HttpStatus { .. }
        | CampaignsOperationsError::InvalidResponse
        | CampaignsOperationsError::ResponseTooLarge
        | CampaignsOperationsError::PaginationLimitExceeded => {
            AppError::dependency_unavailable("Zoho Campaigns could not verify the operation")
        }
    }
}

fn safe_code(raw: &str) -> String {
    let filtered: String = raw
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
        .take(32)
        .collect();
    if filtered.is_empty() {
        "UNKNOWN".into()
    } else {
        filtered
    }
}

async fn active_connections(
    db: &Db,
    ctx: &Ctx,
    only_connection_id: Option<Uuid>,
) -> Result<Vec<marketing_suppressions::ActiveCampaignConnectionRow>, AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    let mut result = Vec::new();
    let mut after = None;
    for page_number in 0..MAX_CONNECTION_PAGES {
        let page = marketing_suppressions::list_active_campaign_connections(
            &mut tx,
            after,
            CONNECTION_PAGE_SIZE,
            only_connection_id,
        )
        .await?;
        if page.is_empty() {
            break;
        }
        let page_len = page.len();
        after = page.last().map(|connection| connection.id);
        result.extend(page);
        if page_len < CONNECTION_PAGE_SIZE as usize {
            break;
        }
        if page_number + 1 == MAX_CONNECTION_PAGES {
            let extra = marketing_suppressions::list_active_campaign_connections(
                &mut tx,
                after,
                1,
                only_connection_id,
            )
            .await?;
            if !extra.is_empty() {
                return Err(AppError::dependency_unavailable(
                    "active Campaigns connection count exceeds the worker limit",
                ));
            }
        }
    }
    tx.commit().await?;
    Ok(result)
}

async fn require_connection_scopes(
    db: &Db,
    ctx: &Ctx,
    connection_id: Uuid,
    required: &[&str],
) -> Result<(), AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    let connection = marketing::get_connection(&mut tx, connection_id)
        .await?
        .ok_or_else(|| AppError::not_found("Campaigns connection not found"))?;
    if connection.status != "active" {
        return Err(AppError::new(
            ErrorCode::IntegrationReauthRequired,
            "Campaigns connection is not active",
        ));
    }
    let scopes = marketing::get_connection_scopes(&mut tx, connection_id).await?;
    tx.commit().await?;
    if required
        .iter()
        .any(|scope| !scopes.iter().any(|granted| granted == scope))
    {
        return Err(AppError::new(
            ErrorCode::IntegrationReauthRequired,
            "Campaigns connection needs updated permissions",
        ));
    }
    Ok(())
}

async fn mark_connection_reauth(db: &Db, ctx: &Ctx, connection_id: Uuid) -> Result<(), AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    if let Some(connection) = marketing::lock_connection(&mut tx, connection_id).await?
        && connection.status == "active"
    {
        marketing::mark_status(
            &mut tx,
            connection_id,
            "reauth_required",
            Some("token_rejected"),
        )
        .await?;
    }
    tx.commit().await
}

async fn persist_push_outcome(
    db: &Db,
    ctx: &Ctx,
    suppression_id: Uuid,
    connection_id: Uuid,
    outcome: Result<CampaignMutationOutcome<()>, CampaignsOperationsError>,
) -> Result<(), AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    match outcome {
        Ok(CampaignMutationOutcome::Accepted(())) => {
            marketing_suppressions::complete_suppression_operation(
                &mut tx,
                suppression_id,
                connection_id,
            )
            .await?;
        }
        Ok(CampaignMutationOutcome::Rejected { code }) => {
            marketing_suppressions::reject_suppression_operation(
                &mut tx,
                suppression_id,
                connection_id,
                &safe_code(&code),
            )
            .await?;
        }
        Ok(CampaignMutationOutcome::Unknown) => {
            marketing_suppressions::mark_suppression_operation_unknown(
                &mut tx,
                suppression_id,
                connection_id,
                Some("provider_outcome_unknown"),
            )
            .await?;
        }
        Err(CampaignsOperationsError::ReauthRequired) => {
            if let Some(connection) = marketing::lock_connection(&mut tx, connection_id).await?
                && connection.status == "active"
            {
                marketing::mark_status(
                    &mut tx,
                    connection_id,
                    "reauth_required",
                    Some("token_rejected"),
                )
                .await?;
            }
            marketing_suppressions::reject_suppression_operation(
                &mut tx,
                suppression_id,
                connection_id,
                "reauth_required",
            )
            .await?;
        }
        Err(
            CampaignsOperationsError::Transport
            | CampaignsOperationsError::InvalidResponse
            | CampaignsOperationsError::ResponseTooLarge,
        ) => {
            marketing_suppressions::mark_suppression_operation_unknown(
                &mut tx,
                suppression_id,
                connection_id,
                Some("provider_outcome_unknown"),
            )
            .await?;
        }
        Err(CampaignsOperationsError::RateLimited { .. }) => {
            marketing_suppressions::reject_suppression_operation(
                &mut tx,
                suppression_id,
                connection_id,
                "rate_limited",
            )
            .await?;
        }
        Err(_) => {
            marketing_suppressions::reject_suppression_operation(
                &mut tx,
                suppression_id,
                connection_id,
                "provider_rejected",
            )
            .await?;
        }
    }
    tx.commit().await
}

/// Push an active Office marketing suppression to every active Campaigns
/// connection. Each Do-Not-Mail write is claimed durably before provider I/O;
/// an existing completed, in-progress, rejected, or unknown claim is never
/// resubmitted.
pub async fn push_suppression(
    db: &Db,
    config: &Config,
    http: &CampaignsHttp,
    operations: &CampaignsOperations,
    ctx: &Ctx,
    email_address_id: Uuid,
) -> Result<(), AppError> {
    push_suppression_to_connection(db, config, http, operations, ctx, email_address_id, None).await
}

pub async fn push_suppression_to_connection(
    db: &Db,
    config: &Config,
    http: &CampaignsHttp,
    operations: &CampaignsOperations,
    ctx: &Ctx,
    email_address_id: Uuid,
    only_connection_id: Option<Uuid>,
) -> Result<(), AppError> {
    require_system(ctx)?;
    let (address, suppressions) = {
        let mut tx = db.begin(ctx.workspace_id).await?;
        if !compliance_core::lock_email_address(&mut tx, email_address_id).await? {
            tx.commit().await?;
            return Ok(());
        }
        let suppressions =
            compliance_core::active_suppressions(&mut tx, email_address_id, "marketing").await?;
        let Some(address) =
            marketing_suppressions::email_address(&mut tx, email_address_id).await?
        else {
            tx.commit().await?;
            return Ok(());
        };
        tx.commit().await?;
        (address.address_original, suppressions)
    };
    if suppressions.is_empty() {
        return Ok(());
    }
    let connections = active_connections(db, ctx, only_connection_id).await?;
    for connection in connections {
        let _account_guard =
            super::lock_connection_operation(db, ctx.workspace_id, connection.id).await?;
        for suppression in &suppressions {
            let claim = {
                let mut tx = db.begin(ctx.workspace_id).await?;
                let claim = marketing_suppressions::claim_suppression_operation(
                    &mut tx,
                    suppression.id,
                    email_address_id,
                    connection.id,
                )
                .await?;
                tx.commit().await?;
                claim
            };
            if claim != Some(OperationClaim::New) {
                continue;
            }

            let tokens = match super::access_tokens(db, ctx, config, http, connection.id).await {
                Ok(tokens) => tokens,
                Err(error) => {
                    if error.code == ErrorCode::IntegrationReauthRequired {
                        mark_connection_reauth(db, ctx, connection.id).await?;
                    }
                    persist_push_outcome(
                        db,
                        ctx,
                        suppression.id,
                        connection.id,
                        Err(CampaignsOperationsError::ReauthRequired),
                    )
                    .await?;
                    continue;
                }
            };
            if let Err(error) =
                require_connection_scopes(db, ctx, connection.id, &["ZohoCampaigns.contact.UPDATE"])
                    .await
            {
                if error.code == ErrorCode::IntegrationReauthRequired {
                    mark_connection_reauth(db, ctx, connection.id).await?;
                }
                persist_push_outcome(
                    db,
                    ctx,
                    suppression.id,
                    connection.id,
                    Err(CampaignsOperationsError::ReauthRequired),
                )
                .await?;
                continue;
            }
            let outcome = operations
                .move_to_do_not_mail(&tokens.access_token, &address)
                .await;
            persist_push_outcome(db, ctx, suppression.id, connection.id, outcome).await?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct ReportActionSummary {
    count: usize,
    from_index: Option<u32>,
    range: Option<u32>,
    partial: bool,
}

async fn record_report_suppressions(
    db: &Db,
    ctx: &Ctx,
    export_id: Uuid,
    recipients: &[marketing_exports::MarketingExportRecipientRow],
    page: &infrastructure::providers::zoho_campaigns::operations::CampaignRecipientPage,
    reason: &str,
    recipient_status: &str,
) -> Result<ReportActionSummary, AppError> {
    let by_normalized: HashMap<String, &marketing_exports::MarketingExportRecipientRow> =
        recipients
            .iter()
            .map(|recipient| (recipient.normalized_address.to_ascii_lowercase(), recipient))
            .collect();
    let mut seen = BTreeSet::new();
    for record in &page.contacts {
        let Ok(normalized) = normalize(&record.email, LocalPartPolicy::Lowercase) else {
            continue;
        };
        let address_match = by_normalized.get(&normalized.normalized_key).copied();
        let email_address_id = if let Some(recipient) = address_match {
            recipient.email_address_id
        } else {
            let mut tx = db.begin(ctx.workspace_id).await?;
            let found =
                marketing_suppressions::find_email_address_id(&mut tx, &normalized.normalized_key)
                    .await?;
            tx.commit().await?;
            let Some(found) = found else {
                continue;
            };
            found
        };
        if !seen.insert(email_address_id) {
            continue;
        }
        let mut tx = db.begin(ctx.workspace_id).await?;
        if !compliance_core::lock_email_address(&mut tx, email_address_id).await? {
            tx.commit().await?;
            continue;
        }
        let active =
            compliance_core::active_suppressions(&mut tx, email_address_id, "marketing").await?;
        if !active
            .iter()
            .any(|suppression| suppression.reason == reason)
        {
            let source_outbound_id = address_match.map(|recipient| recipient.outbound_message_id);
            compliance_core::insert_suppression(
                &mut tx,
                email_address_id,
                "marketing",
                reason,
                "zoho_campaigns_poll",
                Some(ctx.principal_id),
                source_outbound_id,
            )
            .await?;
        }
        if address_match.is_some() {
            marketing_exports::set_recipient_status(
                &mut tx,
                export_id,
                email_address_id,
                recipient_status,
            )
            .await?;
        }
        tx.commit().await?;
    }
    Ok(ReportActionSummary {
        count: page.contacts.len(),
        from_index: Some(page.from_index),
        range: Some(page.range),
        // The documented recipient endpoint exposes page metadata but not a
        // total or request-side cursor, so a single page never proves coverage.
        partial: true,
    })
}

/// Import the complete list-level unsubscribe view, but only for addresses
/// frozen into this Office export. A provider list can contain unrelated
/// contacts, which must not be imported into Office by this sync.
async fn record_list_unsubscribers(
    db: &Db,
    ctx: &Ctx,
    export_id: Uuid,
    recipients: &[marketing_exports::MarketingExportRecipientRow],
    contacts: &[CampaignContactRecord],
) -> Result<ReportActionSummary, AppError> {
    let by_normalized: HashMap<String, &marketing_exports::MarketingExportRecipientRow> =
        recipients
            .iter()
            .map(|recipient| (recipient.normalized_address.to_ascii_lowercase(), recipient))
            .collect();
    let mut seen = BTreeSet::new();
    for contact in contacts {
        let Ok(normalized) = normalize(&contact.email, LocalPartPolicy::Lowercase) else {
            continue;
        };
        let Some(recipient) = by_normalized.get(&normalized.normalized_key).copied() else {
            continue;
        };
        if !seen.insert(recipient.email_address_id) {
            continue;
        }
        let mut tx = db.begin(ctx.workspace_id).await?;
        if !compliance_core::lock_email_address(&mut tx, recipient.email_address_id).await? {
            tx.commit().await?;
            continue;
        }
        let active =
            compliance_core::active_suppressions(&mut tx, recipient.email_address_id, "marketing")
                .await?;
        if !active
            .iter()
            .any(|suppression| suppression.reason == "unsubscribe")
        {
            compliance_core::insert_suppression(
                &mut tx,
                recipient.email_address_id,
                "marketing",
                "unsubscribe",
                "zoho_campaigns_poll",
                Some(ctx.principal_id),
                Some(recipient.outbound_message_id),
            )
            .await?;
        }
        marketing_exports::set_recipient_status(
            &mut tx,
            export_id,
            recipient.email_address_id,
            "unsubscribed",
        )
        .await?;
        tx.commit().await?;
    }
    let mut tx = db.begin(ctx.workspace_id).await?;
    infrastructure::repo::marketing::mark_unsubscribe_checked(&mut tx, export_id).await?;
    tx.commit().await?;
    Ok(ReportActionSummary {
        count: seen.len(),
        from_index: None,
        range: None,
        partial: false,
    })
}

fn report_error_code(error: CampaignsOperationsError) -> String {
    match error {
        CampaignsOperationsError::Rejected { code } => safe_code(&code),
        CampaignsOperationsError::HttpStatus { status } => format!("http_{status}"),
        CampaignsOperationsError::RateLimited { .. } => "rate_limited".into(),
        CampaignsOperationsError::ReauthRequired => "reauth_required".into(),
        CampaignsOperationsError::NotConfigured
        | CampaignsOperationsError::InvalidConfiguration => "not_configured".into(),
        CampaignsOperationsError::InvalidRequest => "invalid_request".into(),
        CampaignsOperationsError::Transport => "transport_error".into(),
        CampaignsOperationsError::InvalidResponse => "invalid_response".into(),
        CampaignsOperationsError::RequestTooLarge => "request_too_large".into(),
        CampaignsOperationsError::ResponseTooLarge => "response_too_large".into(),
        CampaignsOperationsError::PaginationLimitExceeded => "page_limit_exceeded".into(),
    }
}

fn campaign_status_key(status: &str) -> String {
    status
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn next_export_state(
    current: MarketingExportState,
    provider_status: &str,
) -> Option<MarketingExportState> {
    let provider_status = campaign_status_key(provider_status);
    let provider_sent = matches!(provider_status.as_str(), "sent" | "completed");
    let provider_running = matches!(provider_status.as_str(), "inprogress" | "sending");
    let provider_draft = provider_status == "draft";
    match current {
        MarketingExportState::Submitting | MarketingExportState::InProgress => {
            if provider_sent {
                Some(MarketingExportState::Completed)
            } else if provider_running {
                Some(MarketingExportState::InProgress)
            } else {
                Some(MarketingExportState::NeedsReview)
            }
        }
        MarketingExportState::Unknown => {
            if provider_sent {
                Some(MarketingExportState::Completed)
            } else if provider_running {
                Some(MarketingExportState::InProgress)
            } else {
                Some(MarketingExportState::NeedsReview)
            }
        }
        MarketingExportState::Ready if provider_draft => None,
        MarketingExportState::Ready => Some(MarketingExportState::NeedsReview),
        _ => None,
    }
}

fn aggregate_report_value(
    report: &infrastructure::providers::zoho_campaigns::operations::CampaignReport,
) -> Value {
    json!({
        "metrics": report.metrics,
        "details": report.details,
        "reach": report.reach,
        "by_location": report.by_location,
    })
}

#[allow(clippy::too_many_arguments)] // Provider and list coverage counters are separate evidence.
fn report_summary_value(
    aggregate: Option<Value>,
    aggregate_error: Option<String>,
    list_unsubscribers: Option<ReportActionSummary>,
    list_unsubscribers_error: Option<String>,
    optouts: Option<ReportActionSummary>,
    optout_error: Option<String>,
    bounces: Option<ReportActionSummary>,
    bounce_error: Option<String>,
    provider_status: Option<&str>,
) -> Value {
    fn action_value(summary: Option<ReportActionSummary>, error: Option<String>) -> Value {
        match (summary, error) {
            (Some(summary), _) => json!({
                "count": summary.count,
                "from_index": summary.from_index,
                "range": summary.range,
                "partial": summary.partial,
                "coverage": if summary.partial { "partial" } else { "complete" },
            }),
            (None, Some(error)) => json!({"partial": true, "error_code": error}),
            (None, None) => json!({"partial": true, "count": 0}),
        }
    }
    json!({
        "provider_status": provider_status,
        "aggregate": aggregate,
        "aggregate_error": aggregate_error,
        "recipient_reports": {
            "list_unsubscribers": action_value(list_unsubscribers, list_unsubscribers_error),
            "campaign_optouts": action_value(optouts, optout_error),
            "hard_bounces": action_value(bounces, bounce_error),
            "coverage": "partial",
        },
        "observed_at": Utc::now(),
    })
}

async fn cancel_expired_ready_export(
    db: &Db,
    ctx: &Ctx,
    export_id: Uuid,
) -> Result<bool, AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    let Some(current) = marketing_exports::get_export_for_update(&mut tx, export_id).await? else {
        tx.commit().await?;
        return Ok(false);
    };
    if current.state != MarketingExportState::Ready.as_str() {
        tx.commit().await?;
        return Ok(false);
    }
    let launch = campaigns::get_launch_for_update(&mut tx, current.launch_id)
        .await?
        .ok_or_else(|| AppError::internal("marketing export source launch is missing"))?;
    let now = Utc::now();
    if current.expires_at > now && launch.expires_at > now {
        tx.commit().await?;
        return Ok(false);
    }
    if !marketing_exports::update_state(
        &mut tx,
        export_id,
        current.version,
        MarketingExportState::Canceled,
    )
    .await?
    {
        return Err(AppError::conflict(
            "expired marketing export changed concurrently",
        ));
    }
    marketing_exports::update_execution_metadata(
        &mut tx,
        export_id,
        marketing_exports::ExecutionMetadata {
            last_checked_at: Some(Utc::now()),
            report: Some(json!({
                "provider_status": null,
                "office_suppression_check": "expired_ready_export",
                "recipient_reports": {"coverage": "not_queried"},
                "observed_at": Utc::now(),
            })),
            ..Default::default()
        },
    )
    .await?;
    let mut audit = AuditEntry::new(
        ctx.principal_id,
        "marketing.export_canceled_after_expiry",
        "marketing_export",
        ctx.request_id.clone(),
    )
    .with_target(export_id);
    let job_id = match &ctx.auth {
        AuthMethod::System { job_id } => *job_id,
        _ => Uuid::nil(),
    };
    audit.metadata = json!({"reason": "ready_export_expired", "job_id": job_id});
    audit::record(&mut tx, audit).await?;
    tx.commit().await?;
    Ok(true)
}

async fn cancel_ready_for_suppressions(
    db: &Db,
    ctx: &Ctx,
    export_id: Uuid,
    recipients: &[marketing_exports::MarketingExportRecipientRow],
) -> Result<bool, AppError> {
    let mut ordered = recipients.to_vec();
    ordered.sort_by_key(|recipient| recipient.email_address_id);
    let mut tx = db.begin(ctx.workspace_id).await?;
    let mut suppressed = BTreeSet::new();
    for recipient in &ordered {
        if !compliance_core::lock_email_address(&mut tx, recipient.email_address_id).await? {
            continue;
        }
        let active =
            compliance_core::active_suppressions(&mut tx, recipient.email_address_id, "marketing")
                .await?;
        if !active.is_empty() {
            suppressed.insert(recipient.email_address_id);
            marketing_exports::set_recipient_status(
                &mut tx,
                export_id,
                recipient.email_address_id,
                "excluded",
            )
            .await?;
        }
    }
    if suppressed.is_empty() {
        tx.commit().await?;
        return Ok(false);
    }
    let Some(current) = marketing_exports::get_export_for_update(&mut tx, export_id).await? else {
        tx.commit().await?;
        return Ok(false);
    };
    if current.state == MarketingExportState::Ready.as_str() {
        marketing_exports::update_state(
            &mut tx,
            export_id,
            current.version,
            MarketingExportState::Canceled,
        )
        .await?;
        let mut audit = AuditEntry::new(
            ctx.principal_id,
            "marketing.export_canceled_by_suppression",
            "marketing_export",
            ctx.request_id.clone(),
        )
        .with_target(export_id);
        let job_id = match &ctx.auth {
            AuthMethod::System { job_id } => *job_id,
            _ => Uuid::nil(),
        };
        audit.metadata = json!({"excluded_count": suppressed.len(), "job_id": job_id});
        audit::record(&mut tx, audit).await?;
    }
    tx.commit().await?;
    Ok(true)
}

async fn enqueue_next_poll(
    db: &Db,
    ctx: &Ctx,
    export_id: Uuid,
    available_at: chrono::DateTime<Utc>,
    interval: Duration,
) -> Result<(), AppError> {
    let bucket = available_at.timestamp() / interval.num_seconds();
    let mut tx = db.begin(ctx.workspace_id).await?;
    jobs::enqueue(
        &mut tx,
        POLL_KIND,
        1,
        json!({"export_id": export_id}),
        available_at,
        Some(&format!("marketing.poll_export:{export_id}:{bucket}")),
    )
    .await?;
    tx.commit().await
}

/// Poll only read-only Campaigns endpoints. A complete paginated list-level
/// unsubscribe view is primary; campaign recipient opt-out and explicit hard-
/// bounce reports are supplemental and marked partial. Polling never sends or
/// schedules a Campaigns campaign.
pub async fn poll_export(
    db: &Db,
    config: &Config,
    http: &CampaignsHttp,
    operations: &CampaignsOperations,
    ctx: &Ctx,
    export_id: Uuid,
) -> Result<(), AppError> {
    require_system(ctx)?;
    let (initial, _) = load_export(db, ctx.workspace_id, export_id).await?;
    if !pollable_state(&initial.state) {
        return Ok(());
    }
    let _account_guard =
        super::lock_connection_operation(db, ctx.workspace_id, initial.connection_id).await?;
    let (export, recipients) = load_export(db, ctx.workspace_id, export_id).await?;
    let current_state = MarketingExportState::from_str(&export.state)
        .ok_or_else(|| AppError::internal("unknown marketing export state"))?;
    if !pollable_state(&export.state) {
        return Ok(());
    }
    if current_state == MarketingExportState::Ready
        && cancel_expired_ready_export(db, ctx, export_id).await?
    {
        return Ok(());
    }
    if current_state == MarketingExportState::Ready
        && cancel_ready_for_suppressions(db, ctx, export_id, &recipients).await?
    {
        update_poll_metadata(
            db,
            ctx,
            export_id,
            json!({
                "provider_status": null,
                "office_suppression_check": "blocked_ready_export",
                "recipient_reports": {"coverage": "not_queried"},
                "observed_at": Utc::now(),
            }),
        )
        .await?;
        return Ok(());
    }
    let mut required_scopes = Vec::new();
    if export.provider_list_key.is_some() || export.campaign_key.is_some() {
        required_scopes.push("ZohoCampaigns.contact.READ");
    }
    if export.campaign_key.is_some() {
        required_scopes.push("ZohoCampaigns.campaign.READ");
    }
    if !required_scopes.is_empty() {
        require_connection_scopes(db, ctx, export.connection_id, &required_scopes).await?;
    }
    let tokens = if !required_scopes.is_empty() {
        Some(super::access_tokens(db, ctx, config, http, export.connection_id).await?)
    } else {
        None
    };
    let (list_unsubscribers, list_unsubscribers_error) =
        match (export.provider_list_key.as_deref(), tokens.as_ref()) {
            (Some(list_key), Some(tokens)) => match operations
                .get_all_list_contacts(
                    &tokens.access_token,
                    list_key,
                    CampaignContactStatus::Unsubscribed,
                )
                .await
            {
                Ok(contacts) => (
                    Some(
                        record_list_unsubscribers(db, ctx, export_id, &recipients, &contacts)
                            .await?,
                    ),
                    None,
                ),
                Err(error) => {
                    if matches!(&error, CampaignsOperationsError::ReauthRequired) {
                        mark_connection_reauth(db, ctx, export.connection_id).await?;
                    }
                    (None, Some(report_error_code(error)))
                }
            },
            (None, _) => (None, Some("list_key_missing".into())),
            _ => (None, Some("not_configured".into())),
        };
    if current_state == MarketingExportState::Ready
        && cancel_ready_for_suppressions(db, ctx, export_id, &recipients).await?
    {
        let mut summary = report_summary_value(
            None,
            None,
            list_unsubscribers,
            list_unsubscribers_error,
            None,
            None,
            None,
            None,
            None,
        );
        summary["office_suppression_check"] = json!("blocked_ready_export");
        update_poll_metadata(db, ctx, export_id, summary).await?;
        return Ok(());
    }
    let Some(campaign_key) = export.campaign_key.as_deref() else {
        let summary = report_summary_value(
            None,
            None,
            list_unsubscribers,
            list_unsubscribers_error,
            None,
            None,
            None,
            None,
            None,
        );
        update_poll_metadata(db, ctx, export_id, summary).await?;
        if let Some(interval) = poll_interval_for_state(current_state) {
            enqueue_next_poll(db, ctx, export_id, Utc::now() + interval, interval).await?;
        }
        return Ok(());
    };
    let tokens = tokens
        .as_ref()
        .ok_or_else(|| AppError::internal("Campaigns access token was not loaded"))?;
    let details = match operations
        .get_campaign_details(&tokens.access_token, campaign_key, CampaignType::Normal)
        .await
    {
        Ok(details) => details,
        Err(error) => {
            if matches!(&error, CampaignsOperationsError::ReauthRequired) {
                mark_connection_reauth(db, ctx, export.connection_id).await?;
            }
            return Err(campaigns_error(error));
        }
    };
    let provider_status = details.status.as_deref();
    let unknown_send = current_state == MarketingExportState::Unknown;
    let should_read_recipient_reports = !unknown_send
        && provider_status.is_some_and(|status| {
            matches!(
                campaign_status_key(status).as_str(),
                "sent" | "inprogress" | "sending"
            )
        });
    let (aggregate, aggregate_error, optout_summary, optout_error, bounce_summary, bounce_error) =
        if should_read_recipient_reports {
            let (aggregate, aggregate_error) = match operations
                .get_campaign_report(&tokens.access_token, campaign_key)
                .await
            {
                Ok(report) => (Some(aggregate_report_value(&report)), None),
                Err(error) => {
                    if matches!(&error, CampaignsOperationsError::ReauthRequired) {
                        mark_connection_reauth(db, ctx, export.connection_id).await?;
                    }
                    (None, Some(report_error_code(error)))
                }
            };
            let (optout_summary, optout_error) = match operations
                .get_campaign_recipients_data(
                    &tokens.access_token,
                    campaign_key,
                    CampaignRecipientAction::OptOutContacts,
                )
                .await
            {
                Ok(page) => {
                    let summary = record_report_suppressions(
                        db,
                        ctx,
                        export_id,
                        &recipients,
                        &page,
                        "unsubscribe",
                        "unsubscribed",
                    )
                    .await?;
                    (Some(summary), None)
                }
                Err(error) => {
                    if matches!(&error, CampaignsOperationsError::ReauthRequired) {
                        mark_connection_reauth(db, ctx, export.connection_id).await?;
                    }
                    (None, Some(report_error_code(error)))
                }
            };
            let (bounce_summary, bounce_error) = match operations
                .get_campaign_recipients_data(
                    &tokens.access_token,
                    campaign_key,
                    CampaignRecipientAction::SentHardBounce,
                )
                .await
            {
                Ok(page) => {
                    let summary = record_report_suppressions(
                        db,
                        ctx,
                        export_id,
                        &recipients,
                        &page,
                        "hard_bounce",
                        "bounced",
                    )
                    .await?;
                    (Some(summary), None)
                }
                Err(error) => {
                    if matches!(&error, CampaignsOperationsError::ReauthRequired) {
                        mark_connection_reauth(db, ctx, export.connection_id).await?;
                    }
                    (None, Some(report_error_code(error)))
                }
            };
            (
                aggregate,
                aggregate_error,
                optout_summary,
                optout_error,
                bounce_summary,
                bounce_error,
            )
        } else {
            (None, None, None, None, None, None)
        };
    let summary = report_summary_value(
        aggregate,
        aggregate_error,
        list_unsubscribers,
        list_unsubscribers_error,
        optout_summary,
        optout_error,
        bounce_summary,
        bounce_error,
        provider_status,
    );
    let next_state = provider_status.and_then(|status| next_export_state(current_state, status));
    let source_state = provider_status.and_then(export_state_for_provider_status);
    update_poll_state_and_metadata(db, ctx, export_id, next_state, source_state, summary).await?;
    let (latest, _) = load_export(db, ctx.workspace_id, export_id).await?;
    if let Some(state) = MarketingExportState::from_str(&latest.state)
        && let Some(interval) = poll_interval_for_state(state)
    {
        enqueue_next_poll(db, ctx, export_id, Utc::now() + interval, interval).await?;
    }
    Ok(())
}

async fn load_export(
    db: &Db,
    workspace_id: Uuid,
    export_id: Uuid,
) -> Result<
    (
        marketing_exports::MarketingExportRow,
        Vec<marketing_exports::MarketingExportRecipientRow>,
    ),
    AppError,
> {
    let mut tx = db.begin(workspace_id).await?;
    let export = marketing_exports::get_export(&mut tx, export_id)
        .await?
        .ok_or_else(|| AppError::not_found("marketing export not found"))?;
    let recipients = marketing_exports::list_recipients(&mut tx, export_id).await?;
    tx.commit().await?;
    Ok((export, recipients))
}

fn pollable_state(state: &str) -> bool {
    matches!(
        state,
        "ready" | "submitting" | "in_progress" | "completed" | "unknown"
    )
}

fn poll_interval_for_state(state: MarketingExportState) -> Option<Duration> {
    match state {
        MarketingExportState::Ready | MarketingExportState::InProgress => Some(POLL_INTERVAL),
        // Terminal Campaigns sends can receive opt-outs hours or days later.
        // Continue reading their provider list without allowing content TTL
        // to disable suppression synchronization.
        MarketingExportState::Completed => Some(COMPLETED_POLL_INTERVAL),
        _ => None,
    }
}

fn export_state_for_provider_status(status: &str) -> Option<MarketingExportState> {
    match campaign_status_key(status).as_str() {
        "sent" | "completed" => Some(MarketingExportState::Completed),
        "inprogress" | "sending" => Some(MarketingExportState::InProgress),
        _ => None,
    }
}

async fn update_poll_metadata(
    db: &Db,
    ctx: &Ctx,
    export_id: Uuid,
    report: Value,
) -> Result<(), AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    marketing_exports::update_execution_metadata(
        &mut tx,
        export_id,
        marketing_exports::ExecutionMetadata {
            last_checked_at: Some(Utc::now()),
            report: Some(report),
            preserve_send_scan: true,
            ..Default::default()
        },
    )
    .await?;
    tx.commit().await
}

async fn update_poll_state_and_metadata(
    db: &Db,
    ctx: &Ctx,
    export_id: Uuid,
    next: Option<MarketingExportState>,
    source_state: Option<MarketingExportState>,
    report: Value,
) -> Result<(), AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    let current = marketing_exports::get_export_for_update(&mut tx, export_id)
        .await?
        .ok_or_else(|| AppError::not_found("marketing export not found"))?;
    if let (Some(next), Some(current_state)) =
        (next, MarketingExportState::from_str(&current.state))
        && current_state != next
        && current_state.can_transition(next)
    {
        marketing_exports::update_state(&mut tx, export_id, current.version, next).await?;
    }
    if let Some(source_state) = source_state {
        super::delivery::advance_source_launch_for_bulk_tx(
            &mut tx,
            ctx,
            current.launch_id,
            export_id,
            source_state,
        )
        .await?;
    }
    marketing_exports::update_execution_metadata(
        &mut tx,
        export_id,
        marketing_exports::ExecutionMetadata {
            last_checked_at: Some(Utc::now()),
            report: Some(report),
            preserve_send_scan: true,
            ..Default::default()
        },
    )
    .await?;
    tx.commit().await
}
