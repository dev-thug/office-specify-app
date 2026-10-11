//! Workspace-level Zoho Mail OAuth client settings.
//!
//! Admins enter the Zoho API Console client ID, client secret and callback URI
//! in Office Settings instead of editing server environment variables. The
//! non-secret values live in `zoho_mail_oauth_settings`; the client secret is
//! stored only in the encrypted SecretStore and is never returned.
//!
//! [`effective_config`] overlays the saved values onto the process `Config`
//! (saved values win over `ZOHO_MAIL_*` environment values), so the OAuth
//! start/callback flow and the live mailbox router keep consuming a plain
//! `Config` and the existing `MailOAuthConfig::validate` rules.

use contracts::mailboxes::{ZohoOAuthSettingsRequest, ZohoOAuthSettingsResponse};
use contracts::{AppError, Ctx, Violation};
use infrastructure::audit::{self, AuditEntry};
use infrastructure::config::Config;
use infrastructure::db::Db;
use infrastructure::providers::SecretValue;
use infrastructure::providers::zoho::{MailOAuthConfig, valid_client_id};
use infrastructure::repo::zoho_oauth_settings as repo;
use infrastructure::secrets::{FileSecretStore, SecretStore};
use uuid::Uuid;

const SECRET_PREFIX: &str = "zoho_mail_oauth_client";
const MAX_SECRET_LEN: usize = 512;

pub fn secret_ref(workspace_id: Uuid) -> String {
    format!("{SECRET_PREFIX}/{workspace_id}")
}

fn store(config: &Config) -> Result<FileSecretStore, AppError> {
    let key = config.secret_store_key.ok_or_else(|| {
        AppError::dependency_unavailable(
            "OUTREACH_SECRET_KEY must be configured to store the Zoho client secret",
        )
    })?;
    Ok(FileSecretStore::new(&config.secrets_file, key))
}

/// Process config with the workspace's saved Zoho Mail client settings
/// applied. Returns the config unchanged for test fixtures and workspaces
/// without saved settings. A saved row whose secret is unreadable yields a
/// config without a client secret, so validation fails closed.
pub async fn effective_config(
    db: &Db,
    workspace_id: Uuid,
    config: &Config,
) -> Result<Config, AppError> {
    let mut effective = config.clone();
    if config.zoho.fake_for_tests {
        return Ok(effective);
    }
    let mut tx = db.begin(workspace_id).await?;
    let row = repo::get(&mut tx).await?;
    tx.rollback().await?;
    let Some(row) = row else {
        return Ok(effective);
    };
    effective.zoho.mail_client_id = Some(row.client_id);
    if let Some(redirect) = row.redirect_uri {
        effective.zoho.mail_redirect_uri = Some(redirect);
    }
    effective.zoho.mail_client_secret = match store(config) {
        Ok(store) => store
            .get(&row.secret_ref)
            .await
            .ok()
            .flatten()
            .filter(|secret| !secret.trim().is_empty())
            .map(SecretValue::new),
        Err(_) => None,
    };
    Ok(effective)
}

/// Callback URI used when the admin leaves it blank: the server-configured
/// `ZOHO_MAIL_REDIRECT_URI`, else one generated from the public origin.
fn default_redirect_uri(config: &Config) -> String {
    config
        .zoho
        .mail_redirect_uri
        .clone()
        .unwrap_or_else(|| generated_redirect_uri(config))
}

fn generated_redirect_uri(config: &Config) -> String {
    let env = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
    let origin = env("PUBLIC_BASE_URL")
        .or_else(|| env("OUTREACH_PUBLIC_BASE_URL"))
        .unwrap_or_else(|| {
            if config.app_env.is_dev_or_test() {
                "http://localhost:3000".into()
            } else {
                format!("http://{}", config.api_addr)
            }
        });
    format!(
        "{}/api/v1/integrations/zoho/callback",
        origin.trim_end_matches('/')
    )
}

fn missing_fields(config: &Config) -> Vec<String> {
    let mail = MailOAuthConfig {
        client_id: config.zoho.mail_client_id.clone(),
        client_secret: config.zoho.mail_client_secret.clone(),
        redirect_uri: config.zoho.mail_redirect_uri.clone(),
        allow_local_redirect: config.zoho.allow_local_redirect,
    };
    match mail.validate() {
        Ok(()) => vec![],
        Err(error) => error
            .violations
            .into_iter()
            .map(|violation| violation.field)
            .collect(),
    }
}

fn response(
    config: &Config,
    effective: &Config,
    saved: Option<&repo::ZohoOAuthSettingsRow>,
) -> ZohoOAuthSettingsResponse {
    let missing = missing_fields(effective);
    let has_env = config.zoho.mail_client_id.is_some() || config.zoho.mail_client_secret.is_some();
    let source = if saved.is_some() {
        "workspace"
    } else if has_env {
        "environment"
    } else {
        "none"
    };
    ZohoOAuthSettingsResponse {
        configured: missing.is_empty(),
        source: source.into(),
        client_id: effective.zoho.mail_client_id.clone(),
        has_secret: effective.zoho.mail_client_secret.is_some(),
        redirect_uri: effective.zoho.mail_redirect_uri.clone(),
        default_redirect_uri: default_redirect_uri(config),
        missing,
        updated_at: saved.map(|row| row.updated_at),
    }
}

async fn current(
    db: &Db,
    workspace_id: Uuid,
    config: &Config,
) -> Result<ZohoOAuthSettingsResponse, AppError> {
    let effective = effective_config(db, workspace_id, config).await?;
    let mut tx = db.begin(workspace_id).await?;
    let saved = repo::get(&mut tx).await?;
    tx.rollback().await?;
    Ok(response(config, &effective, saved.as_ref()))
}

/// `GET /mail-accounts/oauth-settings` (Admin).
pub async fn get_settings(
    db: &Db,
    ctx: &Ctx,
    config: &Config,
) -> Result<ZohoOAuthSettingsResponse, AppError> {
    super::require_admin_audited(
        db,
        ctx,
        "mailbox.oauth_settings.read",
        "zoho_mail_oauth",
        None,
    )
    .await?;
    current(db, ctx.workspace_id, config).await
}

fn invalid(field: &str, code: &str) -> Violation {
    Violation {
        field: field.into(),
        code: code.into(),
    }
}

/// `PUT /mail-accounts/oauth-settings` (Admin). Omitting `client_secret`
/// keeps the stored secret.
pub async fn put_settings(
    db: &Db,
    ctx: &Ctx,
    config: &Config,
    req: &ZohoOAuthSettingsRequest,
) -> Result<ZohoOAuthSettingsResponse, AppError> {
    super::require_admin_audited(
        db,
        ctx,
        "mailbox.oauth_settings.update",
        "zoho_mail_oauth",
        None,
    )
    .await?;
    let client_id = req.client_id.trim();
    let new_secret = req
        .client_secret
        .as_deref()
        .map(str::trim)
        .filter(|secret| !secret.is_empty());
    let redirect_uri = req
        .redirect_uri
        .as_deref()
        .map(str::trim)
        .filter(|uri| !uri.is_empty());

    let mut violations = Vec::new();
    if !valid_client_id(client_id) {
        violations.push(invalid("client_id", "invalid"));
    }
    if let Some(secret) = new_secret
        && (secret.len() > MAX_SECRET_LEN || secret.chars().any(char::is_control))
    {
        violations.push(invalid("client_secret", "invalid"));
    }

    // Existing stored secret (kept when the request omits `client_secret`).
    let existing = {
        let mut tx = db.begin(ctx.workspace_id).await?;
        let row = repo::get(&mut tx).await?;
        tx.rollback().await?;
        row
    };
    let kept_secret = match (&new_secret, &existing) {
        (None, Some(row)) => store(config)?
            .get(&row.secret_ref)
            .await?
            .filter(|secret| !secret.trim().is_empty()),
        _ => None,
    };
    if new_secret.is_none() && kept_secret.is_none() {
        violations.push(invalid("client_secret", "required"));
    }

    // Reuse the runtime validation rules for the callback URI.
    let candidate_redirect = redirect_uri
        .map(str::to_string)
        .unwrap_or_else(|| default_redirect_uri(config));
    let candidate = MailOAuthConfig {
        client_id: Some(client_id.to_string()),
        client_secret: Some(SecretValue::new("placeholder")),
        redirect_uri: Some(candidate_redirect),
        allow_local_redirect: config.zoho.allow_local_redirect,
    };
    if let Err(error) = candidate.validate()
        && error
            .violations
            .iter()
            .any(|violation| violation.field == "redirect_uri")
    {
        violations.push(invalid("redirect_uri", "invalid"));
    }
    if !violations.is_empty() {
        return Err(AppError::validation(
            "Zoho Mail OAuth settings are invalid",
            violations,
        ));
    }

    let secret_ref = existing
        .as_ref()
        .map(|row| row.secret_ref.clone())
        .unwrap_or_else(|| secret_ref(ctx.workspace_id));
    if let Some(secret) = new_secret {
        store(config)?.put(&secret_ref, secret).await?;
    }
    let mut tx = db.begin(ctx.workspace_id).await?;
    repo::upsert(
        &mut tx,
        client_id,
        redirect_uri,
        &secret_ref,
        ctx.principal_id,
    )
    .await?;
    audit::record(
        &mut tx,
        AuditEntry::new(
            ctx.principal_id,
            "mailbox.oauth_settings.update",
            "zoho_mail_oauth",
            ctx.request_id.clone(),
        ),
    )
    .await?;
    tx.commit().await?;
    current(db, ctx.workspace_id, config).await
}
