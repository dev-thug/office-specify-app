//! Live Zoho Mail runtime provider, OAuth exchange, and mailbox-scoped binding.
//!
//! This module only handles correspondence through a connected Mail account.
//! Marketing and unsubscribe automation remain in Office's delivery pipeline.

use super::oauth::{TokenBundle, TokenExchange, mail_oauth_config};
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use contracts::AppError;
use infrastructure::config::Config;
use infrastructure::db::Db;
use infrastructure::providers::zoho_mail_client::{
    ZohoMailClient, ZohoMailClientError, ZohoMailCompose, ZohoMailFormat, ZohoMailListOptions,
    ZohoMessageSort,
};
use infrastructure::providers::{
    MailCapabilities, MailProvider, ProviderError, ProviderMessage, SendOutcome, SendRequest,
    SentLookup,
};
use infrastructure::providers::{
    SecretValue,
    zoho::{OAuthTokens, ZohoMailOAuthClient},
};
use infrastructure::repo::mailboxes as repo;
use infrastructure::secrets::{FileSecretStore, SecretStore};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

/// Real Mail token endpoint adapter. OAuth requests use Mail client settings,
/// while `ZohoMailOAuthClient` supplies the shared Zoho Accounts token protocol.
#[derive(Clone)]
pub(crate) struct MailTokenExchange {
    oauth: ZohoMailOAuthClient,
    redirect_uri: String,
    #[cfg(test)]
    mail_test_base: Option<String>,
}

impl MailTokenExchange {
    pub(crate) fn new(config: &Config) -> Result<Self, AppError> {
        let oauth_config = mail_oauth_config(config)?;
        let redirect_uri = oauth_config.redirect_uri.clone().unwrap_or_default();
        let oauth = ZohoMailOAuthClient::new(oauth_config)?;
        Ok(Self {
            oauth,
            redirect_uri,
            #[cfg(test)]
            mail_test_base: None,
        })
    }

    async fn refresh(&self, refresh_token: &SecretValue) -> Result<OAuthTokens, ProviderError> {
        self.oauth
            .refresh(refresh_token)
            .await
            .map_err(map_oauth_error)
    }

    pub(crate) async fn exchange_for_account(
        &self,
        code: &str,
        redirect_uri: &str,
        region: &str,
        expected_account_id: Option<&str>,
    ) -> Result<TokenBundle, ProviderError> {
        if region != "com" || redirect_uri != self.redirect_uri {
            return Err(ProviderError::ReauthRequired);
        }
        let tokens = self
            .oauth
            .exchange_code(code)
            .await
            .map_err(map_oauth_error)?;
        let client = self.mail_client(tokens.access_token.clone())?;
        let accounts = client
            .list_accounts()
            .await
            .map_err(map_mail_client_error)?;
        let account = accounts
            .into_iter()
            .filter(|account| account.enabled != Some(false))
            .find_map(|account| {
                if expected_account_id.is_some_and(|expected| account.account_id != expected) {
                    return None;
                }
                let address = account
                    .primary_email_address
                    .clone()
                    .or_else(|| account.mailbox_address.clone())
                    .filter(|address| !address.trim().is_empty())?;
                Some((account, address))
            })
            .ok_or_else(|| {
                if expected_account_id.is_some() {
                    ProviderError::ReauthRequired
                } else {
                    ProviderError::unavailable("Zoho Mail returned no enabled account")
                }
            })?;
        let expires_in_seconds = (tokens.expires_at - Utc::now())
            .num_seconds()
            .clamp(1, 3600);
        Ok(TokenBundle {
            access_token: tokens.access_token.expose().to_owned(),
            refresh_token: tokens
                .refresh_token
                .as_ref()
                .map(|secret| secret.expose().to_owned()),
            expires_in_seconds,
            provider_subject: account.0.account_id.clone(),
            provider_account_id: Some(account.0.account_id),
            primary_address: Some(account.1),
            provider_calendar_id: None,
            scopes: domain::mailboxes::ZOHO_MAIL_SCOPES
                .iter()
                .map(|scope| scope.to_string())
                .collect(),
        })
    }

    #[cfg(test)]
    fn for_test(
        oauth_config: infrastructure::providers::zoho::MailOAuthConfig,
        accounts_base: &str,
        mail_base: &str,
    ) -> Result<Self, AppError> {
        let redirect_uri = oauth_config.redirect_uri.clone().unwrap_or_default();
        Ok(Self {
            oauth: ZohoMailOAuthClient::for_test(oauth_config, accounts_base)?,
            redirect_uri,
            mail_test_base: Some(mail_base.to_owned()),
        })
    }

    fn mail_client(&self, token: SecretValue) -> Result<ZohoMailClient, ProviderError> {
        #[cfg(test)]
        if let Some(base) = &self.mail_test_base {
            return ZohoMailClient::with_base_url_for_testing(token, base)
                .map_err(map_mail_client_error);
        }
        ZohoMailClient::new(token).map_err(map_mail_client_error)
    }
}

#[async_trait]
impl TokenExchange for MailTokenExchange {
    async fn exchange(
        &self,
        code: &str,
        redirect_uri: &str,
        region: &str,
    ) -> Result<TokenBundle, ProviderError> {
        self.exchange_for_account(code, redirect_uri, region, None)
            .await
    }
}

#[derive(Serialize, Deserialize)]
struct StoredMailTokens {
    access_token: String,
    refresh_token: Option<String>,
    expires_in_seconds: i64,
    stored_at: DateTime<Utc>,
}

impl StoredMailTokens {
    fn parse(raw: &str) -> Result<Self, ()> {
        let value: Self = serde_json::from_str(raw).map_err(|_| ())?;
        if value.access_token.trim().is_empty()
            || !value
                .access_token
                .bytes()
                .all(|byte| byte.is_ascii_graphic())
            || !(1..=86_400).contains(&value.expires_in_seconds)
            || value.stored_at > Utc::now() + Duration::hours(24)
            || value.refresh_token.as_deref().is_some_and(|token| {
                token.trim().is_empty() || token.bytes().any(|b| b.is_ascii_whitespace())
            })
        {
            return Err(());
        }
        Ok(value)
    }

    fn from_oauth(tokens: OAuthTokens) -> Self {
        let stored_at = Utc::now();
        let expires_in_seconds = (tokens.expires_at - stored_at).num_seconds().clamp(1, 3600);
        Self {
            access_token: tokens.access_token.expose().to_owned(),
            refresh_token: tokens
                .refresh_token
                .as_ref()
                .map(|token| token.expose().to_owned()),
            expires_in_seconds,
            stored_at,
        }
    }

    fn expires_at(&self) -> DateTime<Utc> {
        self.stored_at + Duration::seconds(self.expires_in_seconds)
    }

    fn to_json(&self) -> Result<String, ProviderError> {
        serde_json::to_string(self)
            .map_err(|_| ProviderError::unavailable("mailbox credential could not be encoded"))
    }
}

fn map_oauth_error(error: AppError) -> ProviderError {
    if error.code == contracts::ErrorCode::IntegrationReauthRequired {
        ProviderError::ReauthRequired
    } else {
        ProviderError::unavailable("Zoho Mail OAuth request failed")
    }
}

fn map_mail_client_error(error: ZohoMailClientError) -> ProviderError {
    match error {
        ZohoMailClientError::ReauthRequired => ProviderError::ReauthRequired,
        ZohoMailClientError::Forbidden => ProviderError::ReauthRequired,
        ZohoMailClientError::RateLimited {
            retry_after_seconds,
        } => ProviderError::RateLimited {
            retry_after_seconds: retry_after_seconds.unwrap_or(60),
        },
        ZohoMailClientError::Unavailable { .. } | ZohoMailClientError::Transport => {
            ProviderError::unavailable("Zoho Mail API is temporarily unavailable")
        }
        _ => ProviderError::unavailable("Zoho Mail API returned an unusable response"),
    }
}

async fn mark_connection_reauth(
    db: &Db,
    workspace_id: Uuid,
    connection_id: Uuid,
    account_id: Option<Uuid>,
    code: &str,
) -> Result<(), ProviderError> {
    let mut tx = db
        .begin(workspace_id)
        .await
        .map_err(|_| ProviderError::unavailable("mailbox reauthorization failed"))?;
    if let Some(connection) = repo::get_connection_for_update(&mut tx, connection_id)
        .await
        .map_err(|_| ProviderError::unavailable("mailbox reauthorization failed"))?
        && connection.status != "disabled"
        && connection.status != "reauth_required"
    {
        mark_connection_reauth_in_tx(&mut tx, &connection, account_id, code)
            .await
            .map_err(|_| ProviderError::unavailable("mailbox reauthorization failed"))?;
    }
    tx.commit()
        .await
        .map_err(|_| ProviderError::unavailable("mailbox reauthorization failed"))
}

async fn mark_connection_reauth_in_tx(
    tx: &mut infrastructure::db::Tx<'_>,
    connection: &repo::IntegrationConnectionRow,
    account_id: Option<Uuid>,
    code: &str,
) -> Result<(), AppError> {
    if connection.status == "reauth_required" || connection.status == "disabled" {
        return Ok(());
    }
    repo::mark_connection_reauth(tx, connection.id, code).await?;
    if let Some(account_id) = account_id {
        repo::set_mail_account_status(tx, account_id, "reauth_required").await?;
    } else {
        for id in repo::account_ids_for_connection(tx, connection.id).await? {
            repo::set_mail_account_status(tx, id, "reauth_required").await?;
        }
    }
    infrastructure::outbox::emit(
        tx,
        super::oauth::EVENT_MAILBOX_REAUTH,
        "integration_connection",
        connection.id,
        connection.version,
        serde_json::json!({"connection_id": connection.id}),
    )
    .await?;
    infrastructure::audit::record(
        tx,
        infrastructure::audit::AuditEntry::new(
            connection.connected_by,
            "integration.reauth_required",
            "integration_connection",
            "mail-provider-runtime",
        )
        .with_target(connection.id),
    )
    .await?;
    Ok(())
}

/// Workspace-scoped routing facade. Its unbound operations always fail
/// closed; callers obtain a mailbox-specific adapter through `for_mailbox`.
#[derive(Clone)]
pub struct MailProviderRouter {
    db: Db,
    config: Arc<Config>,
    #[cfg(test)]
    test_oauth: Option<MailTokenExchange>,
}

impl MailProviderRouter {
    pub fn new(db: Db, config: Arc<Config>) -> Self {
        Self {
            db,
            config,
            #[cfg(test)]
            test_oauth: None,
        }
    }

    #[cfg(test)]
    fn for_test(
        db: Db,
        config: Arc<Config>,
        accounts_base: &str,
        mail_base: &str,
    ) -> Result<Self, AppError> {
        let oauth_config = mail_oauth_config(&config)?;
        let test_oauth = MailTokenExchange::for_test(oauth_config, accounts_base, mail_base)?;
        Ok(Self {
            db,
            config,
            test_oauth: Some(test_oauth),
        })
    }

    fn configured(&self) -> bool {
        !self.config.zoho.fake_for_tests
            && self.config.secret_store_key.is_some()
            && super::oauth::live_mail_oauth_configured(&self.config)
    }

    async fn bind_mailbox(
        &self,
        workspace_id: Uuid,
        mail_account_id: Uuid,
    ) -> Result<Arc<dyn MailProvider>, ProviderError> {
        if !self.configured() {
            return Err(ProviderError::NotConfigured);
        }
        #[cfg(test)]
        let oauth = match &self.test_oauth {
            Some(oauth) => oauth.clone(),
            None => {
                MailTokenExchange::new(&self.config).map_err(|_| ProviderError::NotConfigured)?
            }
        };
        #[cfg(not(test))]
        let oauth =
            MailTokenExchange::new(&self.config).map_err(|_| ProviderError::NotConfigured)?;
        let key = self
            .config
            .secret_store_key
            .ok_or(ProviderError::NotConfigured)?;
        let store = FileSecretStore::new(&self.config.secrets_file, key);
        let mut tx = self
            .db
            .begin(workspace_id)
            .await
            .map_err(|_| ProviderError::unavailable("mailbox binding could not be loaded"))?;
        let account = repo::get_mail_account_scoped(&mut tx, mail_account_id)
            .await
            .map_err(|_| ProviderError::unavailable("mailbox binding could not be loaded"))?
            .ok_or(ProviderError::NotConfigured)?;
        if account.provider != "zoho_mail" || account.status != "active" {
            return Err(if account.status == "reauth_required" {
                ProviderError::ReauthRequired
            } else {
                ProviderError::NotConfigured
            });
        }
        let connection = repo::get_connection_for_update(&mut tx, account.connection_id)
            .await
            .map_err(|_| ProviderError::unavailable("mailbox connection could not be loaded"))?
            .ok_or(ProviderError::NotConfigured)?;
        if connection.provider != "zoho_mail"
            || connection.region != "com"
            || connection.provider_subject.as_deref() != Some(account.provider_account_id.as_str())
            || !valid_zoho_id(&account.provider_account_id)
        {
            return Err(ProviderError::NotConfigured);
        }
        if connection.status != "active" {
            return Err(if connection.status == "reauth_required" {
                ProviderError::ReauthRequired
            } else {
                ProviderError::NotConfigured
            });
        }
        if !domain::mailboxes::ZOHO_MAIL_SCOPES
            .iter()
            .all(|scope| connection.scopes.iter().any(|stored| stored == scope))
        {
            mark_connection_reauth_in_tx(&mut tx, &connection, Some(account.id), "scope_mismatch")
                .await
                .map_err(|_| ProviderError::unavailable("mailbox reauthorization failed"))?;
            tx.commit()
                .await
                .map_err(|_| ProviderError::unavailable("mailbox reauthorization failed"))?;
            return Err(ProviderError::ReauthRequired);
        }
        let credentials = match store.get(&connection.secret_ref).await {
            Ok(Some(raw)) => match StoredMailTokens::parse(&raw) {
                Ok(credentials) => credentials,
                Err(()) => {
                    mark_connection_reauth_in_tx(
                        &mut tx,
                        &connection,
                        Some(account.id),
                        "credential_invalid",
                    )
                    .await
                    .map_err(|_| ProviderError::unavailable("mailbox reauthorization failed"))?;
                    tx.commit().await.map_err(|_| {
                        ProviderError::unavailable("mailbox reauthorization failed")
                    })?;
                    return Err(ProviderError::ReauthRequired);
                }
            },
            Ok(None) => {
                mark_connection_reauth_in_tx(
                    &mut tx,
                    &connection,
                    Some(account.id),
                    "credential_missing",
                )
                .await
                .map_err(|_| ProviderError::unavailable("mailbox reauthorization failed"))?;
                tx.commit()
                    .await
                    .map_err(|_| ProviderError::unavailable("mailbox reauthorization failed"))?;
                return Err(ProviderError::ReauthRequired);
            }
            Err(_) => {
                return Err(ProviderError::unavailable(
                    "mailbox credential store failed",
                ));
            }
        };
        let credentials = if credentials.expires_at() <= Utc::now() + Duration::seconds(60) {
            let Some(refresh_token) = credentials.refresh_token.as_ref() else {
                mark_connection_reauth_in_tx(
                    &mut tx,
                    &connection,
                    Some(account.id),
                    "refresh_token_missing",
                )
                .await
                .map_err(|_| ProviderError::unavailable("mailbox reauthorization failed"))?;
                tx.commit()
                    .await
                    .map_err(|_| ProviderError::unavailable("mailbox reauthorization failed"))?;
                return Err(ProviderError::ReauthRequired);
            };
            let refresh_secret = SecretValue::new(refresh_token.clone());
            let refreshed = match oauth.refresh(&refresh_secret).await {
                Ok(tokens) => tokens,
                Err(ProviderError::ReauthRequired) => {
                    mark_connection_reauth_in_tx(
                        &mut tx,
                        &connection,
                        Some(account.id),
                        "refresh_rejected",
                    )
                    .await
                    .map_err(|_| ProviderError::unavailable("mailbox reauthorization failed"))?;
                    tx.commit().await.map_err(|_| {
                        ProviderError::unavailable("mailbox reauthorization failed")
                    })?;
                    return Err(ProviderError::ReauthRequired);
                }
                Err(error) => return Err(error),
            };
            let refreshed_credentials = StoredMailTokens::from_oauth(refreshed);
            let payload = refreshed_credentials.to_json()?;
            store
                .put(&connection.secret_ref, &payload)
                .await
                .map_err(|_| ProviderError::unavailable("mailbox credential store failed"))?;
            repo::mark_connection_active(
                &mut tx,
                connection.id,
                connection.provider_subject.as_deref().unwrap_or_default(),
                &connection.scopes,
            )
            .await
            .map_err(|_| ProviderError::unavailable("mailbox token refresh state failed"))?
            .ok_or_else(|| ProviderError::unavailable("mailbox token refresh state failed"))?;
            refreshed_credentials
        } else {
            credentials
        };
        tx.commit()
            .await
            .map_err(|_| ProviderError::unavailable("mailbox binding could not be loaded"))?;

        let client = oauth.mail_client(SecretValue::new(credentials.access_token.clone()))?;
        let folders = match client.list_folders(&account.provider_account_id).await {
            Ok(folders) => folders,
            Err(error) => {
                let mapped = map_mail_client_error(error.clone());
                if matches!(
                    error,
                    ZohoMailClientError::ReauthRequired | ZohoMailClientError::Forbidden
                ) {
                    self.mark_reauth(
                        workspace_id,
                        account.connection_id,
                        account.id,
                        "api_rejected",
                    )
                    .await?;
                }
                return Err(mapped);
            }
        };
        let inbox = folders
            .into_iter()
            .find(|folder| {
                folder
                    .folder_type
                    .as_deref()
                    .is_some_and(|kind| kind.eq_ignore_ascii_case("inbox"))
                    || folder.folder_name.eq_ignore_ascii_case("inbox")
            })
            .ok_or_else(|| ProviderError::unavailable("Zoho Mail Inbox folder was not found"))?;
        Ok(Arc::new(BoundZohoMailProvider {
            db: self.db.clone(),
            workspace_id,
            mail_account_id,
            connection_id: connection.id,
            account_id: account.provider_account_id,
            inbox_folder_id: inbox.folder_id,
            client,
        }))
    }

    async fn mark_reauth(
        &self,
        workspace_id: Uuid,
        connection_id: Uuid,
        mail_account_id: Uuid,
        code: &str,
    ) -> Result<(), ProviderError> {
        let mut tx = self
            .db
            .begin(workspace_id)
            .await
            .map_err(|_| ProviderError::unavailable("mailbox reauthorization failed"))?;
        if let Some(connection) = repo::get_connection_for_update(&mut tx, connection_id)
            .await
            .map_err(|_| ProviderError::unavailable("mailbox reauthorization failed"))?
            && connection.status != "disabled"
            && connection.status != "reauth_required"
        {
            mark_connection_reauth_in_tx(&mut tx, &connection, Some(mail_account_id), code)
                .await
                .map_err(|_| ProviderError::unavailable("mailbox reauthorization failed"))?;
        }
        tx.commit()
            .await
            .map_err(|_| ProviderError::unavailable("mailbox reauthorization failed"))
    }
}

#[async_trait]
impl MailProvider for MailProviderRouter {
    async fn for_mailbox(
        &self,
        workspace_id: Uuid,
        mail_account_id: Uuid,
    ) -> Result<Option<Arc<dyn MailProvider>>, ProviderError> {
        self.bind_mailbox(workspace_id, mail_account_id)
            .await
            .map(Some)
    }

    fn sync_page_budget(&self) -> usize {
        1
    }

    fn capabilities(&self) -> MailCapabilities {
        MailCapabilities {
            supports_send: self.configured(),
            supports_delta_sync: false,
        }
    }

    async fn list_messages(
        &self,
        _cursor: Option<String>,
        _limit: u32,
    ) -> Result<(Vec<ProviderMessage>, Option<String>), ProviderError> {
        Err(ProviderError::NotConfigured)
    }

    async fn get_message(
        &self,
        _provider_message_id: &str,
    ) -> Result<ProviderMessage, ProviderError> {
        Err(ProviderError::NotConfigured)
    }

    async fn send(&self, _request: SendRequest) -> Result<SendOutcome, ProviderError> {
        Err(ProviderError::NotConfigured)
    }

    async fn lookup_sent(
        &self,
        _correlation_id: &str,
    ) -> Result<Option<SentLookup>, ProviderError> {
        Err(ProviderError::NotConfigured)
    }
}

struct BoundZohoMailProvider {
    db: Db,
    workspace_id: Uuid,
    mail_account_id: Uuid,
    connection_id: Uuid,
    account_id: String,
    inbox_folder_id: String,
    client: ZohoMailClient,
}

impl BoundZohoMailProvider {
    async fn mark_reauth_after_client_error(&self, error: &ZohoMailClientError) {
        if matches!(
            error,
            ZohoMailClientError::ReauthRequired | ZohoMailClientError::Forbidden
        ) {
            let _ = mark_connection_reauth(
                &self.db,
                self.workspace_id,
                self.connection_id,
                Some(self.mail_account_id),
                "api_rejected",
            )
            .await;
        }
    }

    fn capabilities(&self) -> MailCapabilities {
        MailCapabilities {
            supports_send: true,
            supports_delta_sync: false,
        }
    }
}

#[async_trait]
impl MailProvider for BoundZohoMailProvider {
    fn capabilities(&self) -> MailCapabilities {
        BoundZohoMailProvider::capabilities(self)
    }

    fn sync_page_budget(&self) -> usize {
        1
    }

    async fn list_messages(
        &self,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<(Vec<ProviderMessage>, Option<String>), ProviderError> {
        let start = parse_mail_cursor(cursor.as_deref(), &self.inbox_folder_id)?;
        let page_limit = limit.clamp(1, MAX_MAIL_PAGE_SIZE as u32) as u16;
        let options = ZohoMailListOptions {
            start,
            limit: page_limit,
            include_to: true,
            status: None,
            sort_by: Some(ZohoMessageSort::MessageId),
            sort_order: Some(true),
            thread_id: None,
        };
        let summaries = match self
            .client
            .list_messages(&self.account_id, &self.inbox_folder_id, &options)
            .await
        {
            Ok(messages) => messages,
            Err(error) => {
                self.mark_reauth_after_client_error(&error).await;
                return Err(map_mail_client_error(error));
            }
        };
        let mut messages = Vec::with_capacity(summaries.len());
        let mut locations = Vec::with_capacity(summaries.len());
        for summary in summaries {
            let folder_id = summary
                .folder_id
                .as_deref()
                .unwrap_or(&self.inbox_folder_id);
            if !valid_zoho_id(&summary.message_id) || !valid_zoho_id(folder_id) {
                return Err(ProviderError::unavailable(
                    "Zoho Mail returned an invalid message location",
                ));
            }
            let headers = match self
                .client
                .message_headers(&self.account_id, folder_id, &summary.message_id)
                .await
            {
                Ok(headers) => headers.header_content,
                Err(error) => {
                    self.mark_reauth_after_client_error(&error).await;
                    return Err(map_mail_client_error(error));
                }
            };
            let content = match self
                .client
                .message_content(&self.account_id, folder_id, &summary.message_id)
                .await
            {
                Ok(content) => content.content,
                Err(error) => {
                    self.mark_reauth_after_client_error(&error).await;
                    return Err(map_mail_client_error(error));
                }
            };
            locations.push((summary.message_id.clone(), folder_id.to_owned()));
            messages.push(normalize_message(&summary, content, &headers)?);
        }
        let mut tx = self
            .db
            .begin(self.workspace_id)
            .await
            .map_err(|_| ProviderError::unavailable("message locations could not be stored"))?;
        repo::record_provider_message_locations(&mut tx, self.mail_account_id, &locations)
            .await
            .map_err(|_| ProviderError::unavailable("message locations could not be stored"))?;
        tx.commit()
            .await
            .map_err(|_| ProviderError::unavailable("message locations could not be stored"))?;
        let next = cursor_after_mail_page(
            &self.inbox_folder_id,
            start,
            messages.len(),
            page_limit as usize,
        )?;
        Ok((messages, Some(next)))
    }

    async fn get_message(
        &self,
        provider_message_id: &str,
    ) -> Result<ProviderMessage, ProviderError> {
        if !valid_zoho_id(provider_message_id) {
            return Err(ProviderError::NotConfigured);
        }
        let mut tx = self
            .db
            .begin(self.workspace_id)
            .await
            .map_err(|_| ProviderError::unavailable("message location could not be loaded"))?;
        let folder_id = repo::provider_folder_id_for_message(
            &mut tx,
            self.mail_account_id,
            provider_message_id,
        )
        .await
        .map_err(|_| ProviderError::unavailable("message location could not be loaded"))?;
        tx.commit()
            .await
            .map_err(|_| ProviderError::unavailable("message location could not be loaded"))?;
        let folder_id = folder_id.ok_or_else(|| {
            ProviderError::unavailable("message folder is unknown; mailbox sync is required")
        })?;
        let details = match self
            .client
            .message_details(&self.account_id, &folder_id, provider_message_id)
            .await
        {
            Ok(details) => details,
            Err(error) => {
                self.mark_reauth_after_client_error(&error).await;
                return Err(map_mail_client_error(error));
            }
        };
        let content = match self
            .client
            .message_content(&self.account_id, &folder_id, provider_message_id)
            .await
        {
            Ok(content) => content.content,
            Err(error) => {
                self.mark_reauth_after_client_error(&error).await;
                return Err(map_mail_client_error(error));
            }
        };
        let headers = match self
            .client
            .message_headers(&self.account_id, &folder_id, provider_message_id)
            .await
        {
            Ok(headers) => headers.header_content,
            Err(error) => {
                self.mark_reauth_after_client_error(&error).await;
                return Err(map_mail_client_error(error));
            }
        };
        normalize_message(&details, content, &headers)
    }

    async fn send(&self, request: SendRequest) -> Result<SendOutcome, ProviderError> {
        if !provider_headers_supported(&request.headers, &request.correlation_id) {
            return Ok(SendOutcome::Rejected {
                permanent: true,
                code: "UNSUPPORTED_MAIL_HEADER".into(),
            });
        }
        let outbound_id = match Uuid::parse_str(&request.correlation_id) {
            Ok(id) => id,
            Err(_) => {
                return Ok(SendOutcome::Rejected {
                    permanent: true,
                    code: "INVALID_OFFICE_CORRELATION".into(),
                });
            }
        };
        let mut tx = self
            .db
            .begin(self.workspace_id)
            .await
            .map_err(|_| ProviderError::unavailable("reply metadata could not be loaded"))?;
        let target = repo::provider_reply_target(&mut tx, self.mail_account_id, outbound_id)
            .await
            .map_err(|_| ProviderError::unavailable("reply metadata could not be loaded"))?;
        tx.commit()
            .await
            .map_err(|_| ProviderError::unavailable("reply metadata could not be loaded"))?;
        let Some(target) = target else {
            return Ok(SendOutcome::Rejected {
                permanent: true,
                code: "OUTBOUND_NOT_FOUND".into(),
            });
        };
        if target.purpose != "correspondence" {
            return Ok(SendOutcome::Rejected {
                permanent: true,
                code: "MAIL_CORRESPONDENCE_ONLY".into(),
            });
        }
        let compose = compose_from_request(&request);
        let send_result = if target.has_reply_target {
            let Some(provider_message_id) = target.provider_message_id else {
                return Ok(SendOutcome::Rejected {
                    permanent: true,
                    code: "REPLY_TARGET_METADATA_MISSING".into(),
                });
            };
            self.client
                .reply_to_message(&self.account_id, &provider_message_id, &compose)
                .await
        } else if request.in_reply_to.is_some() {
            return Ok(SendOutcome::Rejected {
                permanent: true,
                code: "REPLY_TARGET_MISMATCH".into(),
            });
        } else {
            self.client.send_message(&self.account_id, &compose).await
        };
        match send_result {
            Ok(outcome) => Ok(outcome),
            Err(error @ (ZohoMailClientError::ReauthRequired | ZohoMailClientError::Forbidden)) => {
                self.mark_reauth_after_client_error(&error).await;
                Err(map_mail_client_error(error))
            }
            Err(error @ ZohoMailClientError::RateLimited { .. }) => {
                Err(map_mail_client_error(error))
            }
            Err(
                error @ (ZohoMailClientError::Transport | ZohoMailClientError::Unavailable { .. }),
            ) => Err(map_mail_client_error(error)),
            Err(_) => Ok(SendOutcome::Rejected {
                permanent: true,
                code: "INVALID_MAIL_REQUEST".into(),
            }),
        }
    }

    async fn lookup_sent(
        &self,
        _correlation_id: &str,
    ) -> Result<Option<SentLookup>, ProviderError> {
        // Zoho does not document a provider idempotency key or a search API
        // that proves delivery from our local correlation UUID.
        Ok(None)
    }
}

const MAX_MAIL_PAGE_SIZE: usize = 25;
const MAIL_CURSOR_RESTART: &str = "restart";

#[derive(Serialize, Deserialize)]
struct MailboxCursor {
    v: u8,
    folder_id: String,
    start: u32,
}

fn parse_mail_cursor(cursor: Option<&str>, inbox_folder_id: &str) -> Result<u32, ProviderError> {
    let Some(cursor) = cursor else { return Ok(1) };
    if cursor == MAIL_CURSOR_RESTART {
        return Ok(1);
    }
    if cursor.len() > 256 {
        return Err(ProviderError::NotConfigured);
    }
    let parsed: MailboxCursor =
        serde_json::from_str(cursor).map_err(|_| ProviderError::NotConfigured)?;
    if parsed.v != 1
        || parsed.folder_id != inbox_folder_id
        || !valid_zoho_id(&parsed.folder_id)
        || parsed.start == 0
        || parsed.start > 100_000_000
    {
        return Err(ProviderError::NotConfigured);
    }
    Ok(parsed.start)
}

fn encode_mail_cursor(folder_id: &str, start: u32) -> Result<String, ProviderError> {
    if !valid_zoho_id(folder_id) || start == 0 || start > 100_000_000 {
        return Err(ProviderError::NotConfigured);
    }
    serde_json::to_string(&MailboxCursor {
        v: 1,
        folder_id: folder_id.to_owned(),
        start,
    })
    .map_err(|_| ProviderError::unavailable("mailbox cursor could not be encoded"))
}

fn cursor_after_mail_page(
    folder_id: &str,
    start: u32,
    count: usize,
    limit: usize,
) -> Result<String, ProviderError> {
    if count < limit {
        Ok(MAIL_CURSOR_RESTART.to_owned())
    } else {
        encode_mail_cursor(
            folder_id,
            start.saturating_add(u32::try_from(count).unwrap_or(u32::MAX)),
        )
    }
}

fn compose_from_request(request: &SendRequest) -> ZohoMailCompose {
    let (content, mail_format) = match (&request.body_html, &request.body_text) {
        (Some(html), _) => (html.clone(), ZohoMailFormat::Html),
        (None, Some(text)) => (text.clone(), ZohoMailFormat::PlainText),
        (None, None) => (String::new(), ZohoMailFormat::PlainText),
    };
    ZohoMailCompose {
        from_address: request.from.clone(),
        to_address: request.to.clone(),
        cc_address: None,
        bcc_address: None,
        subject: request.subject.clone(),
        content,
        mail_format,
        ask_receipt: None,
    }
}

fn provider_headers_supported(headers: &[(String, String)], correlation_id: &str) -> bool {
    headers.iter().all(|(name, value)| {
        name.eq_ignore_ascii_case("X-Outreach-Correlation") && value == correlation_id
    })
}

fn normalize_message(
    summary: &infrastructure::providers::zoho_mail_client::ZohoMessageSummary,
    content: String,
    raw_headers: &str,
) -> Result<ProviderMessage, ProviderError> {
    if !valid_zoho_id(&summary.message_id) {
        return Err(ProviderError::unavailable(
            "Zoho Mail returned an invalid message ID",
        ));
    }
    let from = summary
        .from_address
        .as_deref()
        .and_then(|value| extract_addresses(value).into_iter().next())
        .filter(|address| valid_mail_address(address))
        .ok_or_else(|| ProviderError::unavailable("Zoho Mail message sender is unavailable"))?;
    let message_at = summary
        .received_time
        .as_deref()
        .or(summary.sent_date_in_gmt.as_deref())
        .and_then(|value| value.parse::<i64>().ok())
        .and_then(chrono::DateTime::from_timestamp_millis)
        .ok_or_else(|| ProviderError::unavailable("Zoho Mail message timestamp is invalid"))?;
    let (body_text, body_html) = if content.contains('<') && content.contains('>') {
        (summary.summary.clone(), Some(content))
    } else {
        (Some(content), None)
    };
    let to = summary
        .to_address
        .as_deref()
        .map(extract_addresses)
        .unwrap_or_default();
    let parsed_headers =
        infrastructure::providers::mail_headers::parse_message_headers(raw_headers);
    Ok(ProviderMessage {
        provider_message_id: summary.message_id.clone(),
        provider_thread_id: summary.thread_id.clone(),
        internet_message_id: parsed_headers.internet_message_id,
        in_reply_to: parsed_headers.in_reply_to,
        references: parsed_headers.references,
        from,
        to,
        subject: summary.subject.clone(),
        body_text,
        body_html,
        message_at,
    })
}

fn extract_addresses(raw: &str) -> Vec<String> {
    let decoded = raw
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&");
    let mut result = Vec::new();
    let mut rest = decoded.as_str();
    while let Some(open) = rest.find('<') {
        let after_open = &rest[open + 1..];
        let Some(close) = after_open.find('>') else {
            break;
        };
        let candidate = after_open[..close].trim();
        if valid_mail_address(candidate) {
            result.push(candidate.to_owned());
        }
        rest = &after_open[close + 1..];
    }
    if result.is_empty() {
        for piece in decoded.split([',', ';']) {
            let candidate = piece.trim().trim_matches(['<', '>', '"', '\'']);
            if valid_mail_address(candidate) {
                result.push(candidate.to_owned());
            }
        }
    }
    result
}

fn valid_zoho_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 64 && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn valid_mail_address(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value.len() <= 320
        && value.contains('@')
        && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;
    use infrastructure::providers::{SecretValue, zoho::MailOAuthConfig};
    use std::{net::SocketAddr, time::Duration};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    async fn read_request(stream: &mut tokio::net::TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 1024];
        let mut header_end = None;
        let mut content_length = 0usize;
        loop {
            let count = stream.read(&mut chunk).await.unwrap();
            assert!(count > 0, "mock request closed before its body arrived");
            bytes.extend_from_slice(&chunk[..count]);
            if header_end.is_none()
                && let Some(index) = bytes.windows(4).position(|w| w == b"\r\n\r\n")
            {
                let headers = String::from_utf8_lossy(&bytes[..index]).to_ascii_lowercase();
                content_length = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .and_then(|value| value.trim().parse().ok())
                    .unwrap_or_default();
                header_end = Some(index + 4);
            }
            if header_end.is_some_and(|end| bytes.len() >= end + content_length) {
                return String::from_utf8(bytes).unwrap();
            }
        }
    }

    async fn mock_response(stream: &mut tokio::net::TcpStream, status: &str, body: &str) {
        let response = format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        stream.flush().await.unwrap();
    }

    async fn spawn_mock() -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let request = read_request(&mut stream).await;
                if request.starts_with("POST /oauth/v2/token ") {
                    assert!(request.contains("grant_type=authorization_code"));
                    assert!(request.contains("code=local-code"));
                    mock_response(
                        &mut stream,
                        "200 OK",
                        r#"{"access_token":"local-mail-access","refresh_token":"local-mail-refresh","expires_in":3600}"#,
                    )
                    .await;
                } else {
                    assert!(request.starts_with("GET /api/accounts "));
                    assert!(
                        request
                            .to_ascii_lowercase()
                            .contains("authorization: zoho-oauthtoken local-mail-access")
                    );
                    mock_response(
                        &mut stream,
                        "200 OK",
                        r#"{"status":{"code":200,"description":"success"},"data":[{"accountId":"111","primaryEmailAddress":"disabled@example.test","enabled":false},{"accountId":"222","primaryEmailAddress":"team@example.test","mailboxAddress":"team@example.test","accountDisplayName":"Team inbox","enabled":true}]}"#,
                    )
                    .await;
                }
            }
        });
        (address, handle)
    }

    async fn spawn_refresh_mock() -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_request(&mut stream).await;
            assert!(request.starts_with("POST /oauth/v2/token "));
            assert!(request.contains("grant_type=refresh_token"));
            assert!(request.contains("refresh_token=old-refresh-token"));
            mock_response(
                &mut stream,
                "200 OK",
                r#"{"access_token":"new-mail-access","expires_in":3600}"#,
            )
            .await;
        });
        (address, handle)
    }

    async fn spawn_router_refresh_mock() -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let request = read_request(&mut stream).await;
                if request.starts_with("POST /oauth/v2/token ") {
                    assert!(request.contains("grant_type=refresh_token"));
                    assert!(request.contains("refresh_token=old-refresh-token"));
                    mock_response(
                        &mut stream,
                        "200 OK",
                        r#"{"access_token":"new-mail-access","expires_in":3600}"#,
                    )
                    .await;
                } else {
                    assert!(request.starts_with("GET /api/accounts/222/folders "));
                    assert!(
                        request
                            .to_ascii_lowercase()
                            .contains("authorization: zoho-oauthtoken new-mail-access")
                    );
                    mock_response(
                        &mut stream,
                        "200 OK",
                        r#"{"status":{"code":200,"description":"success"},"data":[{"folderId":"333","folderName":"Inbox","folderType":"Inbox"}]}"#,
                    )
                    .await;
                }
            }
        });
        (address, handle)
    }

    #[tokio::test]
    async fn real_mail_code_exchange_uses_local_zoho_mocks_and_discovers_enabled_account() {
        let (address, server) = spawn_mock().await;
        let base = format!("http://{address}");
        let config = MailOAuthConfig {
            client_id: Some("1000.local-mail-client".into()),
            client_secret: Some(SecretValue::new("local-mail-secret")),
            redirect_uri: Some("http://localhost:3000/api/v1/integrations/zoho/callback".into()),
            allow_local_redirect: true,
        };
        let exchanger = MailTokenExchange::for_test(config, &base, &base).unwrap();

        let result = exchanger
            .exchange(
                "local-code",
                "http://localhost:3000/api/v1/integrations/zoho/callback",
                "com",
            )
            .await
            .unwrap();
        assert_eq!(result.access_token, "local-mail-access");
        assert_eq!(result.refresh_token.as_deref(), Some("local-mail-refresh"));
        assert_eq!(result.provider_subject, "222");
        assert_eq!(result.provider_account_id.as_deref(), Some("222"));
        assert_eq!(result.primary_address.as_deref(), Some("team@example.test"));
        assert_eq!(
            result.scopes,
            domain::mailboxes::ZOHO_MAIL_SCOPES
                .iter()
                .map(|scope| scope.to_string())
                .collect::<Vec<_>>()
        );

        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn refresh_keeps_the_existing_refresh_token_when_zoho_omits_a_new_one() {
        let (address, server) = spawn_refresh_mock().await;
        let base = format!("http://{address}");
        let config = MailOAuthConfig {
            client_id: Some("1000.local-mail-client".into()),
            client_secret: Some(SecretValue::new("local-mail-secret")),
            redirect_uri: Some("http://localhost:3000/api/v1/integrations/zoho/callback".into()),
            allow_local_redirect: true,
        };
        let exchanger = MailTokenExchange::for_test(config, &base, &base).unwrap();

        let refreshed = exchanger
            .refresh(&SecretValue::new("old-refresh-token"))
            .await
            .unwrap();
        assert_eq!(refreshed.access_token.expose(), "new-mail-access");
        assert_eq!(
            refreshed.refresh_token.as_ref().map(SecretValue::expose),
            Some("old-refresh-token")
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn router_refreshes_an_expired_mailbox_token_under_the_connection_lock() {
        let Ok(database_url) = std::env::var("DATABASE_URL") else {
            eprintln!("DATABASE_URL not set — skipping Mail token refresh integration test");
            return;
        };
        let owner = Db::connect(&database_url, None).await.unwrap();
        owner.migrate().await.unwrap();
        let db = Db::connect(&database_url, Some(infrastructure::db::ROLE_API))
            .await
            .unwrap();
        let world = infrastructure::testing::seed_workspace(&db).await.unwrap();
        let secret_path =
            std::env::temp_dir().join(format!("zoho-mail-router-test-{}.json", Uuid::new_v4()));
        let store = FileSecretStore::new(&secret_path, [9; 32]);
        let secret_ref = format!("mail-test:{}", world.connection_id);
        let stored = StoredMailTokens {
            access_token: "expired-mail-access".into(),
            refresh_token: Some("old-refresh-token".into()),
            expires_in_seconds: 3600,
            stored_at: Utc::now() - chrono::Duration::hours(2),
        };
        store
            .put(&secret_ref, &stored.to_json().unwrap())
            .await
            .unwrap();
        let scopes = domain::mailboxes::ZOHO_MAIL_SCOPES
            .iter()
            .map(|scope| scope.to_string())
            .collect::<Vec<_>>();
        let mut tx = db.begin(world.workspace_id).await.unwrap();
        sqlx::query("UPDATE integration_connections SET provider_subject='222',region='com',secret_ref=$3,scopes=$4,status='active' WHERE workspace_id=$1 AND id=$2")
            .bind(world.workspace_id)
            .bind(world.connection_id)
            .bind(&secret_ref)
            .bind(&scopes)
            .execute(tx.conn())
            .await
            .unwrap();
        sqlx::query("UPDATE mail_accounts SET provider_account_id='222',status='active' WHERE workspace_id=$1 AND id=$2")
            .bind(world.workspace_id)
            .bind(world.mail_account_id)
            .execute(tx.conn())
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let (address, server) = spawn_router_refresh_mock().await;
        let base = format!("http://{address}");
        let mut config = Config::test();
        config.zoho.fake_for_tests = false;
        config.zoho.mail_client_id = Some("1000.local-mail-client".into());
        config.zoho.mail_client_secret = Some(SecretValue::new("local-mail-secret"));
        config.zoho.mail_redirect_uri =
            Some("http://localhost:3000/api/v1/integrations/zoho/callback".into());
        config.zoho.allow_local_redirect = true;
        config.secret_store_key = Some([9; 32]);
        config.secrets_file = secret_path.clone();
        let router =
            MailProviderRouter::for_test(db.clone(), Arc::new(config), &base, &base).unwrap();

        let bound = router
            .for_mailbox(world.workspace_id, world.mail_account_id)
            .await
            .unwrap()
            .expect("active Zoho mailbox should bind");
        assert!(bound.capabilities().supports_send);
        let refreshed =
            StoredMailTokens::parse(&store.get(&secret_ref).await.unwrap().unwrap()).unwrap();
        assert_eq!(refreshed.access_token, "new-mail-access");
        assert_eq!(
            refreshed.refresh_token.as_deref(),
            Some("old-refresh-token")
        );

        let mut tx = db.begin(world.workspace_id).await.unwrap();
        let connection = repo::get_connection(&mut tx, world.connection_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(connection.status, "active");
        assert!(connection.last_checked_at.is_some());
        tx.rollback().await.unwrap();
        server.await.unwrap();
        let _ = tokio::fs::remove_file(&secret_path).await;
        let _ = tokio::fs::remove_file(secret_path.with_extension("lock")).await;
    }

    #[test]
    fn mailbox_cursor_is_folder_bound_numeric_and_restarts_after_a_short_page() {
        assert_eq!(parse_mail_cursor(None, "123").unwrap(), 1);
        let next = encode_mail_cursor("123", 26).unwrap();
        assert_eq!(parse_mail_cursor(Some(&next), "123").unwrap(), 26);
        assert!(
            parse_mail_cursor(Some("{\"v\":1,\"folder_id\":\"999\",\"start\":2}"), "123").is_err()
        );
        assert!(
            parse_mail_cursor(Some("{\"v\":1,\"folder_id\":\"123\",\"start\":0}"), "123").is_err()
        );
        assert_eq!(cursor_after_mail_page("123", 26, 8, 25).unwrap(), "restart");
        assert!(cursor_after_mail_page("123", 26, 25, 25).unwrap() != "restart");
    }

    #[tokio::test]
    async fn router_does_not_advertise_send_without_secret_store_key() {
        let mut config = Config::test();
        config.zoho.fake_for_tests = false;
        config.zoho.mail_client_id = Some("1000.local-mail-client".into());
        config.zoho.mail_client_secret = Some(SecretValue::new("local-mail-secret"));
        config.zoho.mail_redirect_uri =
            Some("https://office.example/api/v1/integrations/zoho/callback".into());
        config.secret_store_key = None;
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://test:test@127.0.0.1/outreach_test")
            .unwrap();
        let router = MailProviderRouter::new(Db::new(pool, None, None), Arc::new(config));
        assert!(!router.capabilities().supports_send);
        let request = SendRequest {
            correlation_id: Uuid::new_v4().to_string(),
            from: "sender@example.test".into(),
            to: "recipient@example.test".into(),
            subject: "test".into(),
            body_text: Some("test".into()),
            body_html: None,
            internet_message_id: None,
            in_reply_to: None,
            references: vec![],
            headers: vec![],
        };
        assert!(matches!(
            router.send(request).await,
            Err(ProviderError::NotConfigured)
        ));

        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://test:test@127.0.0.1/outreach_test")
            .unwrap();
        let mut configured = Config::test();
        configured.zoho.fake_for_tests = false;
        configured.zoho.mail_client_id = Some("1000.local-mail-client".into());
        configured.zoho.mail_client_secret = Some(SecretValue::new("local-mail-secret"));
        configured.zoho.mail_redirect_uri =
            Some("https://office.example/api/v1/integrations/zoho/callback".into());
        let router =
            MailProviderRouter::new(Db::new(pool, None, None), Arc::new(configured.clone()));
        assert!(router.capabilities().supports_send);
        configured.zoho.fake_for_tests = true;
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://test:test@127.0.0.1/outreach_test")
            .unwrap();
        let fake = MailProviderRouter::new(Db::new(pool, None, None), Arc::new(configured));
        assert!(!fake.capabilities().supports_send);
    }

    #[test]
    fn zoho_message_normalization_keeps_provider_ids_and_escaped_addresses() {
        let summary = infrastructure::providers::zoho_mail_client::ZohoMessageSummary {
            message_id: "123456789".into(),
            folder_id: Some("987654321".into()),
            thread_id: Some("222".into()),
            from_address: Some("&quot;Alice&quot;&lt;alice@example.test&gt;".into()),
            to_address: Some(
                "&quot;Bob&quot;&lt;bob@example.test&gt;, &quot;Carol&quot;&lt;carol@example.test&gt;"
                    .into(),
            ),
            cc_address: None,
            subject: Some("Hello".into()),
            summary: Some("Snippet".into()),
            received_time: Some("1700000000000".into()),
            sent_date_in_gmt: None,
            has_attachment: Some("0".into()),
        };
        let normalized = normalize_message(
            &summary,
            "<p>Full content</p>".into(),
            "Message-ID: <reply@sender.example>\r\nIn-Reply-To: <sent@office.example>\r\nReferences: <root@office.example> <sent@office.example>\r\n\r\n",
        )
        .unwrap();
        assert_eq!(normalized.provider_message_id, "123456789");
        assert_eq!(normalized.provider_thread_id.as_deref(), Some("222"));
        assert_eq!(normalized.from, "alice@example.test");
        assert_eq!(
            normalized.to,
            vec![
                "bob@example.test".to_owned(),
                "carol@example.test".to_owned()
            ]
        );
        assert_eq!(
            normalized.internet_message_id.as_deref(),
            Some("<reply@sender.example>")
        );
        assert_eq!(
            normalized.in_reply_to.as_deref(),
            Some("<sent@office.example>")
        );
        assert_eq!(
            normalized.references,
            vec!["<root@office.example>", "<sent@office.example>"]
        );
        assert_eq!(normalized.body_html.as_deref(), Some("<p>Full content</p>"));
        assert_eq!(normalized.body_text.as_deref(), Some("Snippet"));
        assert_eq!(normalized.message_at.timestamp_millis(), 1_700_000_000_000);
    }

    #[test]
    fn only_the_matching_office_correlation_header_is_dropped() {
        let correlation = "7e3ea8a6-3a89-4a71-b95d-9629cd1e56c2";
        assert!(provider_headers_supported(
            &[("X-Outreach-Correlation".into(), correlation.into())],
            correlation
        ));
        assert!(!provider_headers_supported(
            &[("List-Unsubscribe".into(), "<https://example.test/u>".into())],
            correlation
        ));
        assert!(!provider_headers_supported(
            &[("X-Outreach-Correlation".into(), "different-id".into())],
            correlation
        ));
    }
}
