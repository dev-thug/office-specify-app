//! Zoho adapter skeletons (spec §4.9): compile-checked trait impls that
//! return `ProviderError::Unavailable` unless a configured credential is
//! supplied. Real API calls are a later module task; the `reqwest` client is
//! constructed here so the dependency shape is proven.

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use super::*;

#[derive(Clone, Debug, Default)]
pub struct ZohoSettings {
    pub mail_client_id: Option<String>,
    pub mail_client_secret: Option<SecretValue>,
    pub mail_redirect_uri: Option<String>,
    pub allow_local_redirect: bool,
    /// Only explicit test config enables the deterministic Mail/Calendar fixture.
    pub fake_for_tests: bool,
}

impl ZohoSettings {
    pub fn from_env(public_origin: &str, allow_local_redirect: bool) -> Self {
        let env = |name: &str| {
            std::env::var(name)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        Self {
            mail_client_id: env("ZOHO_MAIL_CLIENT_ID").or_else(|| env("OUTREACH_ZOHO_CLIENT_ID")),
            mail_client_secret: env("ZOHO_MAIL_CLIENT_SECRET").map(SecretValue::new),
            mail_redirect_uri: env("ZOHO_MAIL_REDIRECT_URI").or_else(|| {
                Some(format!(
                    "{}/api/v1/integrations/zoho/callback",
                    public_origin.trim_end_matches('/')
                ))
            }),
            allow_local_redirect,
            fake_for_tests: false,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct MailOAuthConfig {
    pub client_id: Option<String>,
    pub client_secret: Option<SecretValue>,
    pub redirect_uri: Option<String>,
    pub allow_local_redirect: bool,
}

impl MailOAuthConfig {
    pub fn validate(&self) -> Result<(), contracts::AppError> {
        let valid_id = self.client_id.as_deref().is_some_and(valid_client_id);
        let has_secret = self
            .client_secret
            .as_ref()
            .is_some_and(|secret| !secret.expose().trim().is_empty());
        let valid_uri = self.redirect_uri.as_deref().is_some_and(|value| {
            valid_callback(value) && (self.allow_local_redirect || valid_public_callback(value))
        });
        if valid_id && has_secret && valid_uri {
            return Ok(());
        }
        Err(contracts::AppError::validation(
            "Zoho Mail OAuth configuration is incomplete",
            ["client_id", "client_secret", "redirect_uri"]
                .into_iter()
                .zip([valid_id, has_secret, valid_uri])
                .filter(|(_, valid)| !valid)
                .map(|(field, _)| contracts::Violation {
                    field: field.into(),
                    code: "configuration_required".into(),
                })
                .collect(),
        )
        .with_status(422))
    }
}

pub fn valid_client_id(value: &str) -> bool {
    value.starts_with("1000.")
        && value.len() > 5
        && value.len() <= 200
        && value
            .bytes()
            .all(|character| character.is_ascii_alphanumeric() || b"._-".contains(&character))
}

fn valid_callback(value: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(value) else {
        return false;
    };
    let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    (url.scheme() == "https" || url.scheme() == "http" && local)
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
}

fn valid_public_callback(value: &str) -> bool {
    reqwest::Url::parse(value).is_ok_and(|url| {
        url.scheme() == "https"
            && !matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
    })
}

pub type ProviderUrl = reqwest::Url;

#[derive(Clone, Debug)]
pub struct OAuthTokens {
    pub access_token: SecretValue,
    pub refresh_token: Option<SecretValue>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

impl OAuthTokens {
    pub fn to_secret_json(&self) -> String {
        serde_json::json!({
            "access_token": self.access_token.expose(),
            "refresh_token": self.refresh_token.as_ref().map(SecretValue::expose),
            "expires_at": self.expires_at,
        })
        .to_string()
    }

    pub fn from_secret_json(raw: &str) -> Result<Self, contracts::AppError> {
        let value: serde_json::Value = serde_json::from_str(raw)
            .map_err(|_| contracts::AppError::internal("stored Zoho Mail credential is invalid"))?;
        let access_token = zoho_mail_secret_field(&value, "access_token")?;
        let refresh_token = value["refresh_token"].as_str().map(SecretValue::new);
        let expires_at = value["expires_at"]
            .as_str()
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.with_timezone(&chrono::Utc))
            .ok_or_else(|| {
                contracts::AppError::internal("stored Zoho Mail credential expiry is invalid")
            })?;
        Ok(Self {
            access_token,
            refresh_token,
            expires_at,
        })
    }
}

#[derive(Clone)]
pub struct ZohoMailOAuthClient {
    client: reqwest::Client,
    config: MailOAuthConfig,
    accounts: reqwest::Url,
}

impl ZohoMailOAuthClient {
    pub fn new(config: MailOAuthConfig) -> Result<Self, contracts::AppError> {
        Self::build(config, "https://accounts.zoho.com/")
    }

    fn build(config: MailOAuthConfig, accounts: &str) -> Result<Self, contracts::AppError> {
        config.validate()?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(std::time::Duration::from_secs(20))
            .connect_timeout(std::time::Duration::from_secs(5))
            .build()
            .map_err(|_| {
                contracts::AppError::internal("Zoho Mail HTTP client could not be initialized")
            })?;
        Ok(Self {
            client,
            config,
            accounts: reqwest::Url::parse(accounts)
                .map_err(|_| contracts::AppError::internal("invalid Zoho Accounts host"))?,
        })
    }

    #[cfg(feature = "testing")]
    pub fn for_test(config: MailOAuthConfig, accounts: &str) -> Result<Self, contracts::AppError> {
        let url = reqwest::Url::parse(accounts)
            .map_err(|_| contracts::AppError::internal("invalid test OAuth host"))?;
        if url.scheme() != "http"
            || !matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
        {
            return Err(contracts::AppError::internal(
                "test OAuth host must be loopback HTTP",
            ));
        }
        Self::build(config, &format!("{}/", accounts.trim_end_matches('/')))
    }

    pub async fn exchange_code(&self, code: &str) -> Result<OAuthTokens, contracts::AppError> {
        if code.is_empty() || code.len() > 4096 {
            return Err(zoho_mail_reauth());
        }
        self.token_request(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", self.config.redirect_uri.as_deref().unwrap()),
        ])
        .await
    }

    pub async fn refresh(
        &self,
        refresh_token: &SecretValue,
    ) -> Result<OAuthTokens, contracts::AppError> {
        let mut tokens = self
            .token_request(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token.expose()),
            ])
            .await?;
        if tokens.refresh_token.is_none() {
            tokens.refresh_token = Some(refresh_token.clone());
        }
        Ok(tokens)
    }

    async fn token_request(
        &self,
        params: &[(&str, &str)],
    ) -> Result<OAuthTokens, contracts::AppError> {
        let mut form = params.to_vec();
        form.push(("client_id", self.config.client_id.as_deref().unwrap()));
        form.push((
            "client_secret",
            self.config.client_secret.as_ref().unwrap().expose(),
        ));
        let response = self
            .client
            .post(
                self.accounts
                    .join("oauth/v2/token")
                    .expect("fixed OAuth path"),
            )
            .form(&form)
            .send()
            .await
            .map_err(|_| {
                contracts::AppError::dependency_unavailable("Zoho token endpoint is unavailable")
            })?;
        if response.status().as_u16() == 400 {
            return Err(zoho_mail_reauth());
        }
        let value = zoho_mail_bounded_json(response).await?;
        if value.get("error").is_some() {
            return Err(zoho_mail_reauth());
        }
        let access_token = zoho_mail_secret_field(&value, "access_token")?;
        let refresh_token = value["refresh_token"]
            .as_str()
            .filter(|value| !value.is_empty())
            .map(SecretValue::new);
        let seconds = value["expires_in"]
            .as_u64()
            .or_else(|| {
                value["expires_in"]
                    .as_str()
                    .and_then(|value| value.parse().ok())
            })
            .filter(|seconds: &u64| *seconds > 0 && *seconds <= 86400)
            .ok_or_else(|| {
                contracts::AppError::dependency_unavailable("Zoho returned an invalid token expiry")
            })?;
        Ok(OAuthTokens {
            access_token,
            refresh_token,
            expires_at: chrono::Utc::now() + chrono::Duration::seconds(seconds.min(3600) as i64),
        })
    }
}

fn zoho_mail_secret_field(
    value: &serde_json::Value,
    field: &str,
) -> Result<SecretValue, contracts::AppError> {
    value[field]
        .as_str()
        .filter(|secret| {
            !secret.is_empty() && secret.len() <= 16384 && !secret.chars().any(char::is_whitespace)
        })
        .map(SecretValue::new)
        .ok_or_else(|| {
            contracts::AppError::dependency_unavailable("Zoho returned invalid credentials")
        })
}

fn zoho_mail_reauth() -> contracts::AppError {
    contracts::AppError::new(
        contracts::ErrorCode::IntegrationReauthRequired,
        "Zoho Mail authorization failed; verify the OAuth client, redirect URI and consent",
    )
}

async fn zoho_mail_bounded_json(
    mut response: reqwest::Response,
) -> Result<serde_json::Value, contracts::AppError> {
    if matches!(response.status().as_u16(), 401 | 403) {
        return Err(zoho_mail_reauth());
    }
    if response.status().as_u16() == 429 {
        let seconds = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(60);
        return Err(contracts::AppError::new(
            contracts::ErrorCode::RateLimited,
            "Zoho token endpoint rate limit reached",
        )
        .with_retry_after(seconds));
    }
    if !response.status().is_success() {
        return Err(contracts::AppError::dependency_unavailable(
            "Zoho returned an unsuccessful HTTP response",
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| contracts::AppError::dependency_unavailable("Zoho response was interrupted"))?
    {
        if bytes.len() + chunk.len() > 1024 * 1024 {
            return Err(contracts::AppError::dependency_unavailable(
                "Zoho response exceeded the allowed size",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| contracts::AppError::dependency_unavailable("Zoho returned invalid JSON"))
}

/// Zoho Mail adapter. `credential_ref` is the `provider_accounts` row key —
/// token resolution happens in the mailbox module, not here.
pub struct ZohoMail {
    credential_ref: Option<String>,
    http: reqwest::Client,
    datacenter_base: String,
}

impl ZohoMail {
    /// `datacenter_base` e.g. "https://mail.zoho.com" (region-specific).
    pub fn new(credential_ref: Option<String>, datacenter_base: impl Into<String>) -> Self {
        Self {
            credential_ref,
            http: reqwest::Client::new(),
            datacenter_base: datacenter_base.into(),
        }
    }

    fn unconfigured<T>(&self) -> Result<T, ProviderError> {
        if self.credential_ref.is_none() {
            // R27: a permanent config fault is `NotConfigured` — never
            // `Unknown`, which would spawn pointless reconciliation.
            Err(ProviderError::NotConfigured)
        } else {
            Err(ProviderError::unavailable(
                "zoho mail adapter not implemented (foundation skeleton)",
            ))
        }
    }
}

#[async_trait]
impl MailProvider for ZohoMail {
    fn capabilities(&self) -> MailCapabilities {
        MailCapabilities {
            supports_send: self.credential_ref.is_some(),
            supports_delta_sync: false,
        }
    }
    async fn list_messages(
        &self,
        _cursor: Option<String>,
        _limit: u32,
    ) -> Result<(Vec<ProviderMessage>, Option<String>), ProviderError> {
        let _ = &self.http;
        self.unconfigured()
    }
    async fn get_message(&self, _id: &str) -> Result<ProviderMessage, ProviderError> {
        self.unconfigured()
    }
    async fn send(&self, _req: SendRequest) -> Result<SendOutcome, ProviderError> {
        let _ = (&self.http, &self.datacenter_base);
        self.unconfigured()
    }
    async fn lookup_sent(
        &self,
        _correlation_id: &str,
    ) -> Result<Option<SentLookup>, ProviderError> {
        self.unconfigured()
    }
}

/// Zoho Calendar adapter skeleton.
pub struct ZohoCalendar {
    credential_ref: Option<String>,
    http: reqwest::Client,
}

impl ZohoCalendar {
    pub fn new(credential_ref: Option<String>) -> Self {
        Self {
            credential_ref,
            http: reqwest::Client::new(),
        }
    }
    fn unavailable<T>(&self) -> CalOutcome<T> {
        let _ = &self.http;
        CalOutcome::Failed {
            code: if self.credential_ref.is_none() {
                // R27: distinct from Unknown — a permanent config fault.
                "NOT_CONFIGURED".into()
            } else {
                "DEPENDENCY_UNAVAILABLE".into()
            },
            message: if self.credential_ref.is_none() {
                "zoho calendar not configured".into()
            } else {
                "zoho calendar adapter not implemented".into()
            },
        }
    }
}

#[async_trait]
impl CalendarProvider for ZohoCalendar {
    async fn free_busy(
        &self,
        _host: &str,
        _s: DateTime<Utc>,
        _e: DateTime<Utc>,
    ) -> CalOutcome<Vec<BusyBlock>> {
        self.unavailable()
    }
    async fn create_event(
        &self,
        _host: &str,
        _title: &str,
        _s: DateTime<Utc>,
        _e: DateTime<Utc>,
        _attendee: &str,
    ) -> CalOutcome<CalEvent> {
        self.unavailable()
    }
    async fn update_event(
        &self,
        _host: &str,
        _provider_event_id: &str,
        _title: &str,
        _s: DateTime<Utc>,
        _e: DateTime<Utc>,
    ) -> CalOutcome<CalEvent> {
        self.unavailable()
    }
    async fn cancel_event(&self, _host: &str, _id: &str) -> CalOutcome<()> {
        self.unavailable()
    }
}
