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
