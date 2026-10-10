//! Office-managed SMTP setup and workspace/channel-bound delivery routing.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use contracts::{
    AppError, Ctx, Violation,
    mailboxes::ReviewResult,
    smtp::{CreateSmtpConnectionRequest, SmtpConnectionResponse},
};
use infrastructure::{
    audit::{self, AuditEntry},
    config::Config,
    db::{Db, Tx},
    providers::{
        DeliveryCapabilities, DeliveryProvider, ProviderError, SecretValue, SendOutcome,
        SendRequest, SentLookup,
        smtp::{SmtpConfig, SmtpDeliveryProvider, SmtpSecurity},
    },
    repo::{mailboxes, smtp as repo},
    secrets::{FileSecretStore, SecretStore},
};
use uuid::Uuid;

const SMTP_SECRET_PREFIX: &str = "secret://smtp";

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{valid_email, validate_custom_headers};

    #[test]
    fn rejects_case_insensitive_transport_owned_headers() {
        let mut headers = BTreeMap::new();
        headers.insert("lIsT-uNsUbScRiBe".into(), "https://attacker.invalid".into());
        assert!(validate_custom_headers(&headers).is_err());
    }

    #[test]
    fn rejects_header_values_that_can_inject_a_second_header() {
        let mut headers = BTreeMap::new();
        headers.insert(
            "X-Importance".into(),
            "high\r\nBcc: victim@example.net".into(),
        );
        assert!(validate_custom_headers(&headers).is_err());
    }

    #[test]
    fn accepts_safe_custom_headers() {
        let mut headers = BTreeMap::new();
        headers.insert("X-Importance".into(), "high".into());
        assert!(validate_custom_headers(&headers).is_ok());
    }

    #[test]
    fn rejects_headers_outside_the_transport_allowlist() {
        let mut headers = BTreeMap::new();
        headers.insert("X-Office-Campaign".into(), "autumn-launch".into());
        assert!(validate_custom_headers(&headers).is_err());
    }

    #[test]
    fn rejects_ambiguous_sender_addresses() {
        assert!(!valid_email("news@untrusted@example.test"));
        assert!(!valid_email("news name@example.test"));
    }

    #[test]
    fn rejects_case_colliding_header_names() {
        let headers = BTreeMap::from([
            ("X-Importance".into(), "one".into()),
            ("x-importance".into(), "two".into()),
        ]);
        assert!(validate_custom_headers(&headers).is_err());
    }
}

pub fn secret_ref(workspace_id: Uuid, connection_id: Uuid) -> String {
    format!("{SMTP_SECRET_PREFIX}/{workspace_id}/{connection_id}")
}

pub async fn authorize(db: &Db, ctx: &Ctx, action: &str) -> Result<(), AppError> {
    match crate::marketing::require_admin(ctx) {
        Ok(_) => Ok(()),
        Err(error) => {
            let mut tx = db.begin(ctx.workspace_id).await?;
            audit::record(
                &mut tx,
                AuditEntry::new(
                    ctx.principal_id,
                    action,
                    "smtp_connection",
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

fn secret_store(config: &Config) -> Result<FileSecretStore, AppError> {
    Ok(FileSecretStore::new(
        &config.secrets_file,
        config.secret_store_key.ok_or_else(|| {
            AppError::dependency_unavailable("SMTP credential encryption is not configured")
        })?,
    ))
}

fn response(row: repo::SmtpConnectionRow) -> Result<SmtpConnectionResponse, AppError> {
    let custom_headers = serde_json::from_value::<BTreeMap<String, String>>(row.custom_headers)
        .map_err(|_| AppError::internal("SMTP connection headers are invalid"))?;
    let port = u16::try_from(row.port)
        .map_err(|_| AppError::internal("SMTP connection port is invalid"))?;
    Ok(SmtpConnectionResponse {
        id: row.id,
        name: row.name,
        mail_account_id: row.mail_account_id,
        host: row.host,
        port,
        security: row.security,
        username: row.username,
        from_address: row.from_address,
        from_name: row.from_name,
        reply_to: row.reply_to,
        custom_headers,
        status: row.status,
        last_error_code: row.last_error_code,
        channel_id: row.channel_id,
        sender_identity_id: row.sender_identity_id,
        version: row.version,
        has_password: row.has_password,
    })
}

fn invalid(field: &str, code: &str, message: &str) -> AppError {
    AppError::validation(
        message,
        vec![Violation {
            field: field.into(),
            code: code.into(),
        }],
    )
    .with_status(422)
}

fn valid_host(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 || host.trim() != host {
        return false;
    }
    if host.parse::<std::net::IpAddr>().is_ok() {
        return true;
    }
    if host
        .chars()
        .any(|character| character.is_control() || character.is_whitespace())
    {
        return false;
    }
    host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    })
}

fn valid_email(value: &str) -> bool {
    if value.trim() != value || value.chars().any(char::is_control) {
        return false;
    }
    let Some((local, domain)) = value.split_once('@') else {
        return false;
    };
    if local.is_empty()
        || local.contains('@')
        || domain.is_empty()
        || domain.contains('@')
        || local.len() > 64
        || local.chars().any(char::is_whitespace)
        || local.starts_with('.')
        || local.ends_with('.')
        || local.contains("..")
    {
        return false;
    }
    domain::email_address::normalize(value, domain::email_address::LocalPartPolicy::Preserve)
        .is_ok()
}

fn validate_custom_headers(headers: &BTreeMap<String, String>) -> Result<(), AppError> {
    if headers.len() > 30 {
        return Err(invalid(
            "custom_headers",
            "too_many",
            "at most 30 custom headers are allowed",
        ));
    }

    let mut normalized_names = BTreeSet::new();
    let mut total_bytes = 0usize;
    for (name, value) in headers {
        let lower = name.to_ascii_lowercase();
        let allowed = matches!(lower.as_str(), "x-auto-response-suppress" | "x-importance");
        if !allowed || !normalized_names.insert(lower) {
            return Err(invalid(
                "custom_headers",
                "invalid_header_name",
                "custom header names must be unique safe names and cannot replace transport headers",
            ));
        }
        if value.len() > 998 || value.chars().any(char::is_control) {
            return Err(invalid(
                "custom_headers",
                "invalid_header_value",
                "custom header values must be single-line values under 998 bytes",
            ));
        }
        total_bytes = total_bytes
            .saturating_add(name.len())
            .saturating_add(value.len());
        if total_bytes > 16 * 1024 {
            return Err(invalid(
                "custom_headers",
                "too_large",
                "custom headers exceed the supported size",
            ));
        }
    }
    Ok(())
}

fn validate_request(req: &CreateSmtpConnectionRequest) -> Result<(), AppError> {
    if req.name.trim().is_empty() || req.name.len() > 200 || req.name.chars().any(char::is_control)
    {
        return Err(invalid(
            "name",
            "invalid",
            "name must be 1..=200 characters",
        ));
    }
    if !valid_host(&req.host) {
        return Err(invalid("host", "invalid", "SMTP host is invalid"));
    }
    if req.port == 0 {
        return Err(invalid("port", "invalid", "SMTP port must be positive"));
    }
    if !matches!(req.security.as_str(), "tls" | "starttls") {
        return Err(invalid(
            "security",
            "invalid",
            "security must be tls or starttls",
        ));
    }
    if req.username.trim().is_empty()
        || req.username.len() > 255
        || req.username.chars().any(char::is_control)
    {
        return Err(invalid("username", "invalid", "SMTP username is invalid"));
    }
    if req.password.is_empty() || req.password.len() > 4096 || req.password.contains('\0') {
        return Err(invalid("password", "invalid", "SMTP password is invalid"));
    }
    if !valid_email(&req.from_address) {
        return Err(invalid(
            "from_address",
            "invalid",
            "sender address is invalid",
        ));
    }
    if req.from_name.trim().is_empty()
        || req.from_name.len() > 300
        || req.from_name.chars().any(char::is_control)
    {
        return Err(invalid("from_name", "invalid", "sender name is invalid"));
    }
    if let Some(reply_to) = req.reply_to.as_deref()
        && !valid_email(reply_to)
    {
        return Err(invalid(
            "reply_to",
            "invalid",
            "reply-to address is invalid",
        ));
    }
    if !req.sender_confirmed {
        return Err(invalid(
            "sender_confirmed",
            "required",
            "confirm that this sender address is a registered sender or authorized alias",
        ));
    }
    validate_custom_headers(&req.custom_headers)
}

/// GET `/smtp-connections`; mailbox ACL filters out profiles the administrator
/// cannot send through.
pub async fn list_connections(db: &Db, ctx: &Ctx) -> Result<Vec<SmtpConnectionResponse>, AppError> {
    authorize(db, ctx, "smtp_connection.list").await?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    crate::marketing::revalidate_admin_session_tx(&mut tx, ctx).await?;
    let rows = repo::list_connections(&mut tx, ctx.principal_id).await?;
    tx.commit().await?;
    rows.into_iter().map(response).collect()
}

/// POST `/smtp-connections`, called inside the route's idempotency transaction.
/// The password is encrypted in SecretStore and never returned or audited.
pub async fn create_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    config: &Config,
    id: Uuid,
    req: &CreateSmtpConnectionRequest,
) -> Result<SmtpConnectionResponse, AppError> {
    crate::marketing::revalidate_admin_session_tx(tx, ctx).await?;
    validate_request(req)?;
    let account = mailboxes::get_mail_account_scoped(tx, req.mail_account_id)
        .await?
        .ok_or_else(|| AppError::not_found("mail account not found"))?;
    crate::authz::require_mailbox(
        tx,
        ctx,
        req.mail_account_id,
        crate::authz::MailboxPerm::Send,
    )
    .await?;
    if account.status != "active" {
        return Err(AppError::conflict("mail account is not active"));
    }

    let matching_senders = repo::find_sender_identity(tx, account.id, &req.from_address).await?;
    let sender_identity_id = match matching_senders.as_slice() {
        [sender]
            if sender.address == req.from_address
                && sender.display_name == req.from_name
                && sender.status != "disabled" =>
        {
            repo::verify_sender_identity(tx, account.id, sender.id).await?;
            sender.id
        }
        [] => {
            mailboxes::insert_sender_identity(
                tx,
                account.id,
                &req.from_address,
                &req.from_name,
                true,
            )
            .await?
        }
        _ => {
            return Err(AppError::conflict(
                "an incompatible sender identity already uses this address",
            ));
        }
    };

    let secret_ref = secret_ref(ctx.workspace_id, id);
    repo::insert_pending_integration(tx, id, ctx.principal_id, &secret_ref).await?;
    let channel = mailboxes::insert_delivery_channel(
        tx,
        account.id,
        sender_identity_id,
        id,
        "delivery_provider",
        "marketing",
        req.name.trim(),
        &serde_json::json!({
            "custom_headers": true,
            "one_click_unsubscribe": true,
            "transport": "smtp"
        }),
        None,
    )
    .await?;
    mailboxes::insert_channel_review(
        tx,
        channel.id,
        ctx.principal_id,
        ReviewResult::Allowed.as_str(),
        "marketing",
        None,
        Some(&format!("smtp-sender-attestation:{sender_identity_id}")),
        Some("Administrator attested that this is a registered sender or authorized alias; SMTP verification checks credentials only."),
        None,
    )
    .await?;
    mailboxes::apply_review_result(tx, channel.id, ReviewResult::Allowed.as_str())
        .await?
        .ok_or_else(|| AppError::not_found("SMTP delivery channel not found"))?;

    let custom_headers = serde_json::to_value(&req.custom_headers)
        .map_err(|_| AppError::internal("SMTP custom headers could not be stored"))?;
    let profile = repo::insert_connection(
        tx,
        id,
        account.id,
        sender_identity_id,
        channel.id,
        req.name.trim(),
        &req.host,
        req.port,
        &req.security,
        &req.username,
        &req.from_address,
        &req.from_name,
        req.reply_to.as_deref(),
        &custom_headers,
    )
    .await?;
    audit::record(
        tx,
        AuditEntry {
            metadata: serde_json::json!({
                "mail_account_id": account.id,
                "channel_id": channel.id,
                "sender_identity_id": sender_identity_id,
                "sender_confirmed": true,
                "credential_storage": "encrypted"
            }),
            ..AuditEntry::new(
                ctx.principal_id,
                "smtp_connection.create",
                "smtp_connection",
                ctx.request_id.clone(),
            )
            .with_target(id)
        },
    )
    .await?;

    secret_store(config)?
        .put(&secret_ref, &req.password)
        .await?;
    response(profile)
}

/// POST `/smtp-connections/{id}/verify`. The SMTP adapter only checks TLS and
/// authentication; the sender alias attestation remains an explicit admin
/// review and this path never sends a proof message.
pub async fn verify_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    config: &Config,
    id: Uuid,
) -> Result<SmtpConnectionResponse, AppError> {
    crate::marketing::revalidate_admin_session_tx(tx, ctx).await?;
    let profile = repo::get_for_update(tx, id)
        .await?
        .ok_or_else(|| AppError::not_found("SMTP connection not found"))?;
    crate::authz::require_mailbox(
        tx,
        ctx,
        profile.mail_account_id,
        crate::authz::MailboxPerm::Send,
    )
    .await?;
    if profile.status == "disabled" || profile.integration_status == "disabled" {
        return Err(AppError::conflict("SMTP connection is disabled"));
    }

    let store = secret_store(config)?;
    let Some(password) = store.get(&profile.secret_ref).await? else {
        repo::record_verify_result(
            tx,
            id,
            "reauth_required",
            "reauth_required",
            Some("SMTP_CREDENTIAL_MISSING"),
            false,
        )
        .await?;
        disable_channel(tx, id).await?;
        let updated = repo::get_for_update(tx, id)
            .await?
            .ok_or_else(|| AppError::not_found("SMTP connection not found"))?;
        record_verify_audit(tx, ctx, id, "SMTP_CREDENTIAL_MISSING").await?;
        return response(binding_to_row(updated));
    };

    let smtp = smtp_provider(&profile, &password)?;
    match smtp.verify_connection().await {
        Ok(()) => {
            if profile.channel_policy_status != "allowed" {
                return Err(AppError::conflict("SMTP channel review is not allowed"));
            }
            let row = repo::activate_verified(tx, id).await?.ok_or_else(|| {
                AppError::conflict("SMTP channel review is expired or incomplete")
            })?;
            audit::record(
                tx,
                AuditEntry {
                    metadata: serde_json::json!({
                        "channel_id": profile.channel_id,
                        "credential_check": "passed",
                        "sender_alias_proof": false
                    }),
                    ..AuditEntry::new(
                        ctx.principal_id,
                        "smtp_connection.verify",
                        "smtp_connection",
                        ctx.request_id.clone(),
                    )
                    .with_target(id)
                },
            )
            .await?;
            response(row)
        }
        Err(error) => {
            let (profile_status, integration_status, code, has_password) = match error {
                ProviderError::NotConfigured | ProviderError::ReauthRequired => (
                    "reauth_required",
                    "reauth_required",
                    "SMTP_CREDENTIALS_REJECTED",
                    true,
                ),
                ProviderError::RateLimited { .. } => (
                    profile.status.as_str(),
                    profile.integration_status.as_str(),
                    "SMTP_VERIFY_RATE_LIMITED",
                    true,
                ),
                ProviderError::Unavailable(_) => (
                    profile.status.as_str(),
                    profile.integration_status.as_str(),
                    "SMTP_VERIFY_FAILED",
                    true,
                ),
            };
            repo::record_verify_result(
                tx,
                id,
                profile_status,
                integration_status,
                Some(code),
                has_password,
            )
            .await?;
            if profile_status == "reauth_required" {
                disable_channel(tx, id).await?;
            }
            record_verify_audit(tx, ctx, id, code).await?;
            let updated = repo::get_for_update(tx, id)
                .await?
                .ok_or_else(|| AppError::not_found("SMTP connection not found"))?;
            response(binding_to_row(updated))
        }
    }
}

/// POST `/smtp-connections/{id}/disable`.
pub async fn disable_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    id: Uuid,
) -> Result<SmtpConnectionResponse, AppError> {
    crate::marketing::revalidate_admin_session_tx(tx, ctx).await?;
    let profile = repo::get_for_update(tx, id)
        .await?
        .ok_or_else(|| AppError::not_found("SMTP connection not found"))?;
    crate::authz::require_mailbox(
        tx,
        ctx,
        profile.mail_account_id,
        crate::authz::MailboxPerm::Send,
    )
    .await?;
    let row = repo::disable(tx, id)
        .await?
        .ok_or_else(|| AppError::not_found("SMTP connection not found"))?;
    audit::record(
        tx,
        AuditEntry {
            metadata: serde_json::json!({"channel_id": row.channel_id}),
            ..AuditEntry::new(
                ctx.principal_id,
                "smtp_connection.disable",
                "smtp_connection",
                ctx.request_id.clone(),
            )
            .with_target(id)
        },
    )
    .await?;
    response(row)
}

async fn disable_channel(tx: &mut Tx<'_>, connection_id: Uuid) -> Result<(), AppError> {
    repo::disable_channel(tx, connection_id).await
}

async fn record_verify_audit(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    id: Uuid,
    error_code: &str,
) -> Result<(), AppError> {
    audit::record(
        tx,
        AuditEntry {
            metadata: serde_json::json!({"error_code": error_code}),
            ..AuditEntry::new(
                ctx.principal_id,
                "smtp_connection.verify",
                "smtp_connection",
                ctx.request_id.clone(),
            )
            .with_target(id)
        },
    )
    .await
}

fn binding_to_row(row: repo::SmtpBindingRow) -> repo::SmtpConnectionRow {
    repo::SmtpConnectionRow {
        id: row.id,
        workspace_id: row.workspace_id,
        mail_account_id: row.mail_account_id,
        sender_identity_id: row.sender_identity_id,
        channel_id: row.channel_id,
        name: row.name,
        host: row.host,
        port: row.port,
        security: row.security,
        username: row.username,
        from_address: row.from_address,
        from_name: row.from_name,
        reply_to: row.reply_to,
        custom_headers: row.custom_headers,
        status: row.status,
        last_error_code: row.last_error_code,
        has_password: row.has_password,
        version: row.version,
        created_at: row.created_at,
    }
}

fn smtp_provider(
    profile: &repo::SmtpBindingRow,
    password: &str,
) -> Result<SmtpDeliveryProvider, AppError> {
    let security = match profile.security.as_str() {
        "tls" => SmtpSecurity::Tls,
        "starttls" => SmtpSecurity::Starttls,
        _ => return Err(AppError::internal("SMTP security value is invalid")),
    };
    let custom_headers =
        serde_json::from_value::<BTreeMap<String, String>>(profile.custom_headers.clone())
            .map_err(|_| AppError::internal("SMTP custom headers are invalid"))?;
    SmtpDeliveryProvider::new(SmtpConfig {
        host: profile.host.clone(),
        port: u16::try_from(profile.port)
            .map_err(|_| AppError::internal("SMTP port value is invalid"))?,
        security,
        username: profile.username.clone(),
        password: SecretValue::new(password),
        from_address: profile.from_address.clone(),
        from_name: profile.from_name.clone(),
        reply_to: profile.reply_to.clone(),
        custom_headers,
    })
    .map_err(|_| AppError::dependency_unavailable("SMTP connection settings are invalid"))
}

/// Wraps the existing generic delivery adapter. A configured SMTP connection
/// binds only when the trusted `(workspace_id, channel_id)` lookup matches;
/// absent profiles preserve existing generic and fake adapters.
#[derive(Clone)]
pub struct DeliveryRouter {
    db: Db,
    config: Arc<Config>,
    fallback: Arc<dyn DeliveryProvider>,
}

impl DeliveryRouter {
    pub fn new(db: Db, config: Arc<Config>, fallback: Arc<dyn DeliveryProvider>) -> Self {
        Self {
            db,
            config,
            fallback,
        }
    }
}

#[async_trait::async_trait]
impl DeliveryProvider for DeliveryRouter {
    async fn for_channel(
        &self,
        workspace_id: Uuid,
        channel_id: Uuid,
    ) -> Result<Option<Arc<dyn DeliveryProvider>>, ProviderError> {
        let mut tx = self
            .db
            .begin(workspace_id)
            .await
            .map_err(|_| ProviderError::unavailable("SMTP profile lookup failed"))?;
        let profile = repo::resolve_for_channel(&mut tx, channel_id)
            .await
            .map_err(|_| ProviderError::unavailable("SMTP profile lookup failed"))?;
        let smtp_channel_without_profile = if profile.is_none() {
            repo::channel_uses_smtp_integration(&mut tx, channel_id)
                .await
                .map_err(|_| ProviderError::unavailable("SMTP profile lookup failed"))?
        } else {
            false
        };
        tx.commit()
            .await
            .map_err(|_| ProviderError::unavailable("SMTP profile lookup failed"))?;

        let Some(profile) = profile else {
            if smtp_channel_without_profile {
                return Ok(Some(Arc::new(FailClosedDeliveryProvider)));
            }
            return self.fallback.for_channel(workspace_id, channel_id).await;
        };
        let enabled = profile.workspace_id == workspace_id
            && profile.channel_id == channel_id
            && profile.status == "active"
            && profile.integration_status == "active"
            && profile.channel_enabled
            && profile.channel_policy_status == "allowed"
            && profile.sender_status == "verified"
            && profile.has_password;
        if !enabled {
            return Ok(Some(Arc::new(FailClosedDeliveryProvider)));
        }
        let store = match secret_store(&self.config) {
            Ok(store) => store,
            Err(_) => return Ok(Some(Arc::new(FailClosedDeliveryProvider))),
        };
        let Some(password) = store
            .get(&profile.secret_ref)
            .await
            .map_err(|_| ProviderError::unavailable("SMTP credential lookup failed"))?
        else {
            return Ok(Some(Arc::new(FailClosedDeliveryProvider)));
        };
        match smtp_provider(&profile, &password) {
            Ok(provider) => Ok(Some(Arc::new(provider))),
            Err(_) => Ok(Some(Arc::new(FailClosedDeliveryProvider))),
        }
    }

    fn capabilities(&self) -> DeliveryCapabilities {
        self.fallback.capabilities()
    }

    async fn send(&self, req: SendRequest) -> Result<SendOutcome, ProviderError> {
        self.fallback.send(req).await
    }

    async fn lookup_sent(&self, correlation_id: &str) -> Result<Option<SentLookup>, ProviderError> {
        self.fallback.lookup_sent(correlation_id).await
    }
}

struct FailClosedDeliveryProvider;

#[async_trait::async_trait]
impl DeliveryProvider for FailClosedDeliveryProvider {
    fn capabilities(&self) -> DeliveryCapabilities {
        DeliveryCapabilities::default()
    }

    async fn send(&self, _req: SendRequest) -> Result<SendOutcome, ProviderError> {
        Err(ProviderError::NotConfigured)
    }

    async fn lookup_sent(
        &self,
        _correlation_id: &str,
    ) -> Result<Option<SentLookup>, ProviderError> {
        Ok(None)
    }
}
