//! Zoho Campaigns OAuth and API boundary. Separate from Zoho Mail.
pub mod http;

use contracts::{AppError, Violation};

#[derive(Clone)]
pub struct SecretValue(String);
impl SecretValue {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}

pub const CAMPAIGNS_SCOPES: &[&str] =
    &["ZohoCampaigns.campaign.READ", "ZohoCampaigns.contact.READ"];

#[derive(Clone, Debug, Default)]
pub struct CampaignsOAuthConfig {
    pub client_id: Option<String>,
    pub client_secret: Option<SecretValue>,
    pub redirect_uri: Option<String>,
    pub allow_local_redirect: bool,
}
impl CampaignsOAuthConfig {
    pub fn missing_fields(&self) -> Vec<String> {
        let mut result = Vec::new();
        if !self.client_id.as_deref().is_some_and(valid_client_id) {
            result.push("ZOHO_CAMPAIGNS_CLIENT_ID".into());
        }
        if !self
            .client_secret
            .as_ref()
            .is_some_and(|s| !s.expose().trim().is_empty())
        {
            result.push("ZOHO_CAMPAIGNS_CLIENT_SECRET".into());
        }
        if !self.redirect_uri.as_deref().is_some_and(|value| {
            valid_callback(value) && (self.allow_local_redirect || valid_public_callback(value))
        }) {
            result.push("ZOHO_CAMPAIGNS_REDIRECT_URI".into());
        }
        result
    }
    pub fn validate(&self) -> Result<(), AppError> {
        let missing = self.missing_fields();
        if missing.is_empty() {
            return Ok(());
        }
        Err(AppError::validation(
            "Zoho Campaigns OAuth setup is incomplete; configure the server before connecting",
            missing
                .into_iter()
                .map(|field| Violation {
                    field,
                    code: "configuration_required".into(),
                })
                .collect(),
        )
        .with_status(422))
    }
    pub fn authorization_url(&self, state: &str) -> Result<String, AppError> {
        self.validate()?;
        let mut url =
            reqwest::Url::parse("https://accounts.zoho.com/oauth/v2/auth").expect("fixed URL");
        url.query_pairs_mut()
            .append_pair("client_id", self.client_id.as_deref().unwrap())
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", self.redirect_uri.as_deref().unwrap())
            .append_pair("scope", &CAMPAIGNS_SCOPES.join(","))
            .append_pair("access_type", "offline")
            .append_pair("prompt", "consent")
            .append_pair("state", state);
        Ok(url.to_string())
    }
}
pub fn valid_client_id(value: &str) -> bool {
    value.starts_with("1000.")
        && value.len() > 5
        && value.len() <= 200
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
}
pub fn valid_callback(value: &str) -> bool {
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

#[derive(Clone, Debug, Default)]
pub struct EmailApiSettings {
    pub enabled: bool,
    pub api_key: Option<SecretValue>,
    pub workspace_id: Option<uuid::Uuid>,
}

#[derive(Clone, Debug, Default)]
pub struct ZohoSettings {
    pub mail_client_id: Option<String>,
    pub mail_client_secret: Option<SecretValue>,
    pub mail_redirect_uri: Option<String>,
    pub campaigns: CampaignsOAuthConfig,
    pub email_api: EmailApiSettings,
    /// Only Config::test enables the old deterministic Mail/Calendar fixture.
    pub fake_for_tests: bool,
}
impl ZohoSettings {
    pub fn from_env(public_origin: &str, allow_local_redirect: bool) -> Self {
        let env = |name: &str| {
            std::env::var(name)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        Self {
            mail_client_secret: env("ZOHO_MAIL_CLIENT_SECRET").map(SecretValue::new),
            mail_client_id: env("ZOHO_MAIL_CLIENT_ID").or_else(|| env("OUTREACH_ZOHO_CLIENT_ID")),
            mail_redirect_uri: env("ZOHO_MAIL_REDIRECT_URI").or_else(|| {
                Some(format!(
                    "{}/api/v1/integrations/zoho/callback",
                    public_origin.trim_end_matches('/')
                ))
            }),
            campaigns: CampaignsOAuthConfig {
                allow_local_redirect,
                client_id: env("ZOHO_CAMPAIGNS_CLIENT_ID"),
                client_secret: env("ZOHO_CAMPAIGNS_CLIENT_SECRET").map(SecretValue::new),
                redirect_uri: env("ZOHO_CAMPAIGNS_REDIRECT_URI").or_else(|| {
                    (!public_origin.is_empty()).then(|| {
                        format!(
                            "{}/api/v1/integrations/zoho-campaigns/callback",
                            public_origin.trim_end_matches('/')
                        )
                    })
                }),
            },
            email_api: EmailApiSettings {
                enabled: env("ZOHO_EMAIL_API_ENABLED").as_deref() == Some("true"),
                api_key: env("ZOHO_EMAIL_API_KEY").map(SecretValue::new),
                workspace_id: env("ZOHO_EMAIL_API_WORKSPACE_ID").and_then(|id| id.parse().ok()),
            },
            fake_for_tests: false,
        }
    }
}

pub mod operations;
