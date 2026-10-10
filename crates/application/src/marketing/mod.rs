//! Real Zoho Campaigns connection lifecycle, separate from fake mailbox adapters.
use chrono::{Duration, Utc};
use contracts::marketing::*;
use contracts::{AppError, AuthMethod, Ctx, ErrorCode, PrincipalKind, Role};
use infrastructure::providers::zoho_campaigns::{
    CAMPAIGNS_SCOPES,
    http::{CampaignsHttp, OAuthTokens},
};
use infrastructure::{
    audit::{self, AuditEntry},
    config::Config,
    crypto,
    db::{Db, Tx},
    repo::marketing as repo,
    secrets::{FileSecretStore, SecretStore},
};
use uuid::Uuid;

pub fn require_admin(ctx: &Ctx) -> Result<Uuid, AppError> {
    if ctx.principal_kind != PrincipalKind::Human || ctx.role != Role::Admin {
        return Err(AppError::forbidden(
            "human administrator required for Campaigns connections",
        ));
    }
    match ctx.auth {
        AuthMethod::Session { session_id } => Ok(session_id),
        _ => Err(AppError::forbidden(
            "browser session required for Campaigns connections",
        )),
    }
}
pub async fn authorize(db: &Db, ctx: &Ctx, action: &str) -> Result<Uuid, AppError> {
    match require_admin(ctx) {
        Ok(id) => Ok(id),
        Err(error) => {
            let mut tx = db.begin(ctx.workspace_id).await?;
            audit::record(
                &mut tx,
                AuditEntry::new(
                    ctx.principal_id,
                    action,
                    "integration_connection",
                    ctx.request_id.clone(),
                )
                .denied(&error.code),
            )
            .await?;
            tx.commit().await?;
            Err(error)
        }
    }
}
fn store(config: &Config) -> Result<FileSecretStore, AppError> {
    Ok(FileSecretStore::new(
        &config.secrets_file,
        config.secret_store_key.ok_or_else(|| {
            AppError::dependency_unavailable("secret store encryption is not configured")
        })?,
    ))
}
fn response(row: repo::ConnectionRow) -> MarketingConnectionResponse {
    MarketingConnectionResponse {
        id: row.id,
        write_scopes_ready: CAMPAIGNS_SCOPES
            .iter()
            .all(|scope| row.scopes.iter().any(|granted| granted == scope)),
        name: row.name,
        status: row.status,
        plan: row.plan,
        region: row.region,
        last_verified_at: row.last_verified_at,
        lists_count: row.lists_count,
        created_at: row.created_at,
    }
}
pub async fn setup(
    db: &Db,
    ctx: &Ctx,
    config: &Config,
) -> Result<CampaignsSetupResponse, AppError> {
    require_admin(ctx)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let rows = repo::list_connections(&mut tx).await?;
    tx.commit().await?;
    Ok(CampaignsSetupResponse {
        configured: config.zoho.campaigns.missing_fields().is_empty(),
        missing_fields: config.zoho.campaigns.missing_fields(),
        callback_uri: config
            .zoho
            .campaigns
            .redirect_uri
            .clone()
            .unwrap_or_default(),
        accounts_host: "https://accounts.zoho.com".into(),
        campaigns_host: "https://campaigns.zoho.com".into(),
        plan: "free".into(),
        scheduled_send: false,
        webhooks: false,
        mail_oauth_configured: config.zoho.mail_client_id.is_some(),
        email_api_configured: config.zoho.email_api.enabled
            && config.zoho.email_api.api_key.is_some()
            && config.zoho.email_api.workspace_id == Some(ctx.workspace_id),
        workspace_id: ctx.workspace_id,
        connections: rows.into_iter().map(response).collect(),
    })
}
pub async fn start_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    config: &Config,
) -> Result<MarketingConnectResponse, AppError> {
    let session = require_admin(ctx)?;
    config.zoho.campaigns.validate()?;
    let id = Uuid::new_v4();
    let raw = crypto::generate_token();
    let hash = crypto::sha256_bytes(raw.as_bytes());
    let expires = Utc::now() + Duration::minutes(10);
    let secret_ref = format!("zoho_campaigns:{}:{id}", ctx.workspace_id);
    repo::insert_connection(
        tx,
        id,
        ctx.principal_id,
        &secret_ref,
        &CAMPAIGNS_SCOPES
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>(),
    )
    .await?;
    repo::insert_state(
        tx,
        id,
        ctx.principal_id,
        session,
        &hash,
        config.zoho.campaigns.redirect_uri.as_deref().unwrap(),
        expires,
    )
    .await?;
    audit::record(
        tx,
        AuditEntry::new(
            ctx.principal_id,
            "marketing.oauth_started",
            "integration_connection",
            ctx.request_id.clone(),
        )
        .with_target(id),
    )
    .await?;
    Ok(MarketingConnectResponse {
        connection_id: id,
        authorization_url: Some(config.zoho.campaigns.authorization_url(&raw)?),
        state_expires_at: expires,
    })
}
pub async fn callback(
    db: &Db,
    ctx: &Ctx,
    config: &Config,
    provider: &CampaignsHttp,
    raw_state: &str,
    code: Option<&str>,
    denied: bool,
) -> Result<Uuid, AppError> {
    let session = require_admin(ctx)?;
    config.zoho.campaigns.validate()?;
    if raw_state.len() > 256 {
        return Err(invalid_state());
    }
    let mut tx = db.begin(ctx.workspace_id).await?;
    let state = repo::lock_state(&mut tx, &crypto::sha256_bytes(raw_state.as_bytes()))
        .await?
        .ok_or_else(invalid_state)?;
    if state.used_at.is_some()
        || state.expires_at <= Utc::now()
        || state.principal_id != ctx.principal_id
        || state.session_id != session
        || Some(state.redirect_uri.as_str()) != config.zoho.campaigns.redirect_uri.as_deref()
    {
        return Err(invalid_state());
    }
    let connection = repo::get_connection(&mut tx, state.connection_id)
        .await?
        .ok_or_else(invalid_state)?;
    if connection.status != "pending" {
        return Err(invalid_state());
    }
    repo::consume_state(&mut tx, state.id).await?;
    tx.commit().await?;
    if denied || code.is_none() {
        mark_failure(db, ctx, state.connection_id, "consent_denied").await?;
        return Err(AppError::new(
            ErrorCode::IntegrationReauthRequired,
            "Zoho consent was denied; start a new connection",
        ));
    }
    let result = async {
        let tokens = provider.exchange_code(code.unwrap()).await?;
        if tokens.refresh_token.is_none() {
            return Err(AppError::new(
                ErrorCode::IntegrationReauthRequired,
                "Zoho did not issue a refresh token; authorize again with offline consent",
            ));
        }
        let lists = provider.list_lists(&tokens.access_token).await?;
        let mut tx = db.begin(ctx.workspace_id).await?;
        let current = repo::lock_connection(&mut tx, state.connection_id)
            .await?
            .ok_or_else(invalid_state)?;
        if current.status != "pending" {
            return Err(invalid_state());
        }
        let still_authorized: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM principals p JOIN user_sessions s ON s.workspace_id=p.workspace_id AND s.principal_id=p.id WHERE p.workspace_id=$1 AND p.id=$2 AND p.kind='human' AND p.role='admin' AND p.status='active' AND p.permission_version=$3 AND s.id=$4 AND s.revoked_at IS NULL AND s.expires_at>now() FOR SHARE OF p,s)")
            .bind(ctx.workspace_id).bind(ctx.principal_id).bind(ctx.permission_version).bind(session).fetch_one(tx.conn()).await.map_err(infrastructure::error::map_sqlx)?;
        if !still_authorized { return Err(AppError::forbidden("OAuth owner session or administrator permission was revoked")); }
        store(config)?
            .put(&current.secret_ref, &tokens.to_secret_json())
            .await?;
        repo::mark_status(&mut tx, current.id, "active", None).await?;
        repo::mark_verified(&mut tx, current.id, lists.len() as i32).await?;
        audit::record(
            &mut tx,
            AuditEntry::new(
                ctx.principal_id,
                "marketing.connected",
                "integration_connection",
                ctx.request_id.clone(),
            )
            .with_target(current.id),
        )
        .await?;
        suppressions::enqueue_suppression_backfill_in_tx(&mut tx, current.id).await?;
        tx.commit().await?;
        Ok(current.id)
    }
    .await;
    if result.is_err() {
        mark_failure(db, ctx, state.connection_id, "oauth_exchange_failed").await?;
    }
    result
}
async fn mark_failure(db: &Db, ctx: &Ctx, id: Uuid, code: &str) -> Result<(), AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    let row = repo::lock_connection(&mut tx, id)
        .await?
        .ok_or_else(|| AppError::not_found("Campaigns connection not found"))?;
    if row.status != "disabled" {
        repo::mark_status(&mut tx, id, "reauth_required", Some(code)).await?;
    }
    audit::record(
        &mut tx,
        AuditEntry::new(
            ctx.principal_id,
            "marketing.reauth_required",
            "integration_connection",
            ctx.request_id.clone(),
        )
        .with_target(id),
    )
    .await?;
    tx.commit().await
}
fn invalid_state() -> AppError {
    AppError::validation(
        "invalid, expired or already used Campaigns OAuth state",
        vec![],
    )
    .with_status(422)
}

pub(crate) async fn access_tokens(
    db: &Db,
    ctx: &Ctx,
    config: &Config,
    provider: &CampaignsHttp,
    id: Uuid,
) -> Result<OAuthTokens, AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    // The DB transaction lock also serializes refreshes across API/worker processes.
    let connection = repo::lock_connection(&mut tx, id)
        .await?
        .ok_or_else(|| AppError::not_found("Campaigns connection not found"))?;
    if connection.status != "active" {
        return Err(AppError::new(
            ErrorCode::IntegrationReauthRequired,
            "Campaigns connection is not active",
        ));
    }
    let secret_store = store(config)?;
    let raw = secret_store
        .get(&connection.secret_ref)
        .await?
        .ok_or_else(|| {
            AppError::new(
                ErrorCode::IntegrationReauthRequired,
                "Campaigns credential is missing",
            )
        })?;
    let mut tokens = OAuthTokens::from_secret_json(&raw)?;
    if tokens.expires_at <= Utc::now() + Duration::seconds(60) {
        let refresh = tokens.refresh_token.as_ref().ok_or_else(|| {
            AppError::new(
                ErrorCode::IntegrationReauthRequired,
                "Campaigns refresh token is missing",
            )
        })?;
        match provider.refresh(refresh).await {
            Ok(new) => {
                tokens = new;
                secret_store
                    .put(&connection.secret_ref, &tokens.to_secret_json())
                    .await?;
            }
            Err(error) => {
                if error.code == ErrorCode::IntegrationReauthRequired {
                    repo::mark_status(&mut tx, id, "reauth_required", Some("token_refresh_failed"))
                        .await?;
                    tx.commit().await?;
                }
                return Err(error);
            }
        }
    }
    tx.commit().await?;
    Ok(tokens)
}
pub async fn list_lists(
    db: &Db,
    ctx: &Ctx,
    config: &Config,
    provider: &CampaignsHttp,
    id: Uuid,
) -> Result<Vec<MarketingListResponse>, AppError> {
    require_admin(ctx)?;
    let tokens = access_tokens(db, ctx, config, provider, id).await?;
    let lists = match provider.list_lists(&tokens.access_token).await {
        Ok(lists) => lists,
        Err(error) => {
            if error.code == ErrorCode::IntegrationReauthRequired {
                mark_failure(db, ctx, id, "token_rejected").await?;
            }
            return Err(error);
        }
    };
    let mut tx = db.begin(ctx.workspace_id).await?;
    repo::mark_verified(&mut tx, id, lists.len() as i32).await?;
    tx.commit().await?;
    Ok(lists
        .into_iter()
        .map(|list| MarketingListResponse {
            key: list.key,
            name: list.name,
            contacts: list.contacts,
        })
        .collect())
}
pub async fn disconnect_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    id: Uuid,
) -> Result<MarketingDisconnectedResponse, AppError> {
    require_admin(ctx)?;
    let connection = repo::lock_connection(tx, id)
        .await?
        .ok_or_else(|| AppError::not_found("Campaigns connection not found"))?;
    if connection.status != "disabled" {
        repo::mark_status(tx, id, "disabled", None).await?;
        audit::record(
            tx,
            AuditEntry::new(
                ctx.principal_id,
                "marketing.disconnected",
                "integration_connection",
                ctx.request_id.clone(),
            )
            .with_target(id),
        )
        .await?;
    }
    Ok(MarketingDisconnectedResponse { disconnected: true })
}
/// Cleanup can be retried even after an idempotent DB response was committed.
pub async fn cleanup_disconnected(
    db: &Db,
    ctx: &Ctx,
    config: &Config,
    id: Uuid,
) -> Result<(), AppError> {
    require_admin(ctx)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let connection = repo::lock_connection(&mut tx, id)
        .await?
        .ok_or_else(|| AppError::not_found("Campaigns connection not found"))?;
    if connection.status != "disabled" {
        return Err(AppError::conflict(
            "disable the connection before removing credentials",
        ));
    }
    store(config)?.delete(&connection.secret_ref).await?;
    tx.commit().await
}

pub async fn recent_campaigns(
    db: &Db,
    ctx: &Ctx,
    config: &Config,
    provider: &CampaignsHttp,
    id: Uuid,
) -> Result<Vec<NativeCampaignResponse>, AppError> {
    require_admin(ctx)?;
    let tokens = access_tokens(db, ctx, config, provider, id).await?;
    let campaigns = provider.recent_campaigns(&tokens.access_token).await?;
    Ok(campaigns
        .into_iter()
        .map(|c| NativeCampaignResponse {
            key: c.key,
            name: c.name,
            status: c.status,
        })
        .collect())
}

pub mod email_templates;

pub mod content;

pub async fn sender_identities(
    db: &Db,
    ctx: &Ctx,
    mail_account_id: Uuid,
) -> Result<Vec<MarketingSenderResponse>, AppError> {
    authorize(db, ctx, "marketing.sender_list").await?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    crate::authz::require_mailbox(
        &mut tx,
        ctx,
        mail_account_id,
        crate::authz::MailboxPerm::Read,
    )
    .await?;
    let rows: Vec<(Uuid,String,String,String)> = sqlx::query_as(
        "SELECT id,address,display_name,status FROM sender_identities WHERE workspace_id=$1 AND mail_account_id=$2 ORDER BY address LIMIT 100"
    ).bind(ctx.workspace_id).bind(mail_account_id).fetch_all(tx.conn()).await.map_err(infrastructure::error::map_sqlx)?;
    tx.commit().await?;
    Ok(rows
        .into_iter()
        .map(
            |(id, address, display_name, status)| MarketingSenderResponse {
                id,
                address,
                display_name,
                status,
            },
        )
        .collect())
}

pub mod delivery;

pub async fn authorize_sender_registration(
    db: &Db,
    ctx: &Ctx,
    account: Uuid,
) -> Result<(), AppError> {
    authorize(db, ctx, "marketing.sender_register").await?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    crate::authz::require_mailbox(&mut tx, ctx, account, crate::authz::MailboxPerm::Send).await?;
    tx.commit().await
}
pub async fn register_sender_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    req: &RegisterMarketingSenderRequest,
) -> Result<MarketingSenderResponse, AppError> {
    revalidate_admin_session_tx(tx, ctx).await?;
    crate::authz::require_mailbox(
        tx,
        ctx,
        req.mail_account_id,
        crate::authz::MailboxPerm::Send,
    )
    .await?;
    let address = domain::email_address::normalize(
        &req.address,
        domain::email_address::LocalPartPolicy::Preserve,
    )
    .map_err(|_| AppError::validation("sender address is invalid", vec![]).with_status(422))?
    .original;
    if address.contains(['\r', '\n'])
        || req.display_name.trim().is_empty()
        || req.display_name.contains(['\r', '\n'])
        || req.display_name.len() > 300
        || req.verification_evidence_ref.trim().is_empty()
        || req.verification_evidence_ref.len() > 2000
    {
        return Err(AppError::validation(
            "sender name and recorded Zoho verification evidence are required",
            vec![],
        )
        .with_status(422));
    }
    if repo::find_sender_by_address(tx, req.mail_account_id, &address)
        .await?
        .is_some()
    {
        return Err(AppError::conflict(
            "sender identity already exists; select the existing identity",
        ));
    }
    let id = infrastructure::repo::mailboxes::insert_sender_identity(
        tx,
        req.mail_account_id,
        &address,
        req.display_name.trim(),
        true,
    )
    .await?;
    let mut entry = AuditEntry::new(
        ctx.principal_id,
        "marketing.sender_registered_with_evidence",
        "sender_identity",
        &ctx.request_id,
    )
    .with_target(id);
    entry.metadata = serde_json::json!({"verification_source":"human_recorded_zoho_evidence","evidence_ref":req.verification_evidence_ref,"mail_account_id":req.mail_account_id});
    audit::record(tx, entry).await?;
    Ok(MarketingSenderResponse {
        id,
        address,
        display_name: req.display_name.trim().into(),
        status: "verified".into(),
    })
}

/// Reserve capacity before taking a database-backed account lock: each
/// provider operation journals on another short-lived connection.
static CONNECTION_WRITERS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
pub struct ConnectionOperationGuard<'db> {
    _tx: Tx<'db>,
    _permit: tokio::sync::SemaphorePermit<'static>,
}
pub async fn lock_connection_operation<'db>(
    db: &'db Db,
    workspace_id: Uuid,
    _connection_id: Uuid,
) -> Result<ConnectionOperationGuard<'db>, AppError> {
    let permit = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        CONNECTION_WRITERS.acquire(),
    )
    .await
    .map_err(|_| AppError::dependency_unavailable("Campaigns account writers are busy"))?
    .map_err(|_| AppError::dependency_unavailable("Campaigns account writer unavailable"))?;
    let hash =
        crypto::sha256_bytes(format!("office-campaigns-workspace:{workspace_id}").as_bytes());
    let key = i64::from_be_bytes(
        hash[..8]
            .try_into()
            .map_err(|_| AppError::internal("invalid account lock"))?,
    );
    let mut tx = db.begin(workspace_id).await?;
    let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
        .bind(key)
        .fetch_one(tx.conn())
        .await
        .map_err(infrastructure::error::map_sqlx)?;
    if !locked {
        return Err(AppError::conflict(
            "another operation is updating this Campaigns account",
        ));
    }
    Ok(ConnectionOperationGuard {
        _tx: tx,
        _permit: permit,
    })
}
pub async fn revalidate_admin_session_tx(tx: &mut Tx<'_>, ctx: &Ctx) -> Result<(), AppError> {
    let session = require_admin(ctx)?;
    let active:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM principals p JOIN user_sessions s ON s.workspace_id=p.workspace_id AND s.principal_id=p.id WHERE p.workspace_id=$1 AND p.id=$2 AND p.kind='human' AND p.role='admin' AND p.status='active' AND p.permission_version=$3 AND s.id=$4 AND s.revoked_at IS NULL AND s.expires_at>now() FOR SHARE OF p,s)")
        .bind(ctx.workspace_id).bind(ctx.principal_id).bind(ctx.permission_version).bind(session).fetch_one(tx.conn()).await.map_err(infrastructure::error::map_sqlx)?;
    if !active {
        return Err(AppError::forbidden(
            "Campaigns administrator session or permissions changed",
        ));
    }
    Ok(())
}

pub mod suppressions;

fn mapping_metadata_error(
    error: infrastructure::providers::zoho_campaigns::operations::CampaignsOperationsError,
) -> AppError {
    use infrastructure::providers::zoho_campaigns::operations::CampaignsOperationsError as E;
    match error {
        E::ReauthRequired => AppError::new(
            ErrorCode::IntegrationReauthRequired,
            "Campaigns authorization must be renewed",
        ),
        E::RateLimited {
            retry_after_seconds,
        } => AppError::new(ErrorCode::RateLimited, "Campaigns rate limit reached")
            .with_retry_after(retry_after_seconds.unwrap_or(60)),
        E::InvalidRequest => AppError::validation("invalid Campaigns metadata page", vec![]),
        _ => AppError::dependency_unavailable("Campaigns mapping metadata could not be read"),
    }
}
pub async fn contact_fields(
    db: &Db,
    ctx: &Ctx,
    config: &Config,
    http: Option<&CampaignsHttp>,
    operations: Option<&infrastructure::providers::zoho_campaigns::operations::CampaignsOperations>,
    connection: Uuid,
) -> Result<Vec<MarketingContactFieldResponse>, AppError> {
    authorize(db, ctx, "marketing.contact_fields_read").await?;
    let http =
        http.ok_or_else(|| AppError::dependency_unavailable("Campaigns OAuth is not configured"))?;
    let operations = operations
        .ok_or_else(|| AppError::dependency_unavailable("Campaigns client is not configured"))?;
    let tokens = access_tokens(db, ctx, config, http, connection).await?;
    Ok(operations
        .get_contact_fields(&tokens.access_token)
        .await
        .map_err(mapping_metadata_error)?
        .into_iter()
        .map(|field| MarketingContactFieldResponse {
            display_name: field.display_name,
            field_name: field.field_name,
            field_id: field.field_id,
        })
        .collect())
}
#[allow(clippy::too_many_arguments)]
pub async fn topics(
    db: &Db,
    ctx: &Ctx,
    config: &Config,
    http: Option<&CampaignsHttp>,
    operations: Option<&infrastructure::providers::zoho_campaigns::operations::CampaignsOperations>,
    connection: Uuid,
    from_index: u32,
    limit: u32,
) -> Result<Vec<MarketingTopicResponse>, AppError> {
    authorize(db, ctx, "marketing.topics_read").await?;
    if from_index == 0 || !(1..=100).contains(&limit) || from_index > 10000 {
        return Err(AppError::validation("invalid topic page", vec![]));
    }
    let http =
        http.ok_or_else(|| AppError::dependency_unavailable("Campaigns OAuth is not configured"))?;
    let operations = operations
        .ok_or_else(|| AppError::dependency_unavailable("Campaigns client is not configured"))?;
    let tokens = access_tokens(db, ctx, config, http, connection).await?;
    Ok(operations
        .get_topics(&tokens.access_token, from_index, limit)
        .await
        .map_err(mapping_metadata_error)?
        .into_iter()
        .map(|topic| MarketingTopicResponse {
            id: topic.id,
            name: topic.name,
            primary_list: topic.primary_list,
        })
        .collect())
}

/// Controls follow the send gate's lock order: approval -> export -> launch.
/// A submitted/uncertain provider request cannot be recalled by local controls.
pub async fn lock_native_launch_control_tx(
    tx: &mut Tx<'_>,
    launch_id: Uuid,
) -> Result<Option<infrastructure::repo::marketing_exports::MarketingExportRow>, AppError> {
    let Some(launch) = infrastructure::repo::campaigns::get_launch(tx, launch_id).await? else {
        return Ok(None);
    };
    if !infrastructure::repo::campaigns::get_channel(tx, launch.delivery_channel_id)
        .await?
        .is_some_and(|channel| channel.provider == "zoho_campaigns")
    {
        return Ok(None);
    }
    if let Some(approval) =
        infrastructure::repo::approvals::active_request_for_launch(tx, launch_id).await?
    {
        infrastructure::repo::approvals::get_request_for_update(tx, approval.id).await?;
    }
    let Some(export) =
        infrastructure::repo::marketing_exports::get_export_by_launch(tx, launch_id).await?
    else {
        return Ok(None);
    };
    let export = infrastructure::repo::marketing_exports::get_export_for_update(tx, export.id)
        .await?
        .ok_or_else(|| AppError::not_found("native export not found"))?;
    let submitted = infrastructure::repo::marketing_exports::get_operation(
        tx,
        export.id,
        "send_campaign",
        "send_campaign",
    )
    .await?
    .is_some_and(|operation| {
        matches!(
            operation.status.as_str(),
            "started" | "unknown" | "completed"
        )
    });
    if submitted
        || matches!(
            export.state.as_str(),
            "submitting" | "in_progress" | "completed"
        )
    {
        return Err(AppError::conflict(
            "Zoho Campaigns submission has started; local pause, cancel or revoke cannot recall it",
        ));
    }
    Ok(Some(export))
}
pub async fn cancel_unsubmitted_native_export_tx(
    tx: &mut Tx<'_>,
    export: Option<&infrastructure::repo::marketing_exports::MarketingExportRow>,
) -> Result<(), AppError> {
    if let Some(export) = export {
        let state = domain::marketing::MarketingExportState::from_str(&export.state)
            .ok_or_else(|| AppError::internal("invalid native export state"))?;
        if state != domain::marketing::MarketingExportState::Canceled
            && state.can_transition(domain::marketing::MarketingExportState::Canceled)
        {
            infrastructure::repo::marketing_exports::update_state(
                tx,
                export.id,
                export.version,
                domain::marketing::MarketingExportState::Canceled,
            )
            .await?;
        }
    }
    Ok(())
}
