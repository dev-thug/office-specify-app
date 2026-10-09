use super::{CampaignsOAuthConfig, SecretValue};
use chrono::{DateTime, Duration, Utc};
use contracts::{AppError, ErrorCode};
use reqwest::{Client, Response, Url};
pub type ProviderUrl = Url;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub struct OAuthTokens {
    pub access_token: SecretValue,
    pub refresh_token: Option<SecretValue>,
    pub expires_at: DateTime<Utc>,
}
impl OAuthTokens {
    pub fn to_secret_json(&self) -> String {
        json!({"access_token":self.access_token.expose(),"refresh_token":self.refresh_token.as_ref().map(SecretValue::expose),"expires_at":self.expires_at}).to_string()
    }
    pub fn from_secret_json(raw: &str) -> Result<Self, AppError> {
        let value: Value = serde_json::from_str(raw)
            .map_err(|_| AppError::internal("stored Campaigns credential is invalid"))?;
        let access = secret_field(&value, "access_token")?;
        let refresh_token = value["refresh_token"].as_str().map(SecretValue::new);
        let expires_at = value["expires_at"]
            .as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc))
            .ok_or_else(|| AppError::internal("stored Campaigns credential expiry is invalid"))?;
        Ok(Self {
            access_token: access,
            refresh_token,
            expires_at,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CampaignList {
    pub key: String,
    pub name: String,
    pub contacts: u64,
}

#[derive(Clone, Debug)]
pub struct NativeCampaign {
    pub key: String,
    pub name: String,
    pub status: String,
}

#[derive(Clone)]
pub struct CampaignsHttp {
    client: Client,
    config: CampaignsOAuthConfig,
    accounts: Url,
    api: Url,
}
impl CampaignsHttp {
    pub fn new(config: CampaignsOAuthConfig) -> Result<Self, AppError> {
        Self::build(
            config,
            "https://accounts.zoho.com/",
            "https://campaigns.zoho.com/",
        )
    }
    fn build(config: CampaignsOAuthConfig, accounts: &str, api: &str) -> Result<Self, AppError> {
        config.validate()?;
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(std::time::Duration::from_secs(20))
            .connect_timeout(std::time::Duration::from_secs(5))
            .build()
            .map_err(|_| AppError::internal("Campaigns HTTP client could not be initialized"))?;
        Ok(Self {
            client,
            config,
            accounts: Url::parse(accounts)
                .map_err(|_| AppError::internal("invalid accounts host"))?,
            api: Url::parse(api).map_err(|_| AppError::internal("invalid Campaigns host"))?,
        })
    }
    #[cfg(feature = "testing")]
    pub fn for_test(config: CampaignsOAuthConfig, base: &str) -> Result<Self, AppError> {
        let url = Url::parse(base).map_err(|_| AppError::internal("invalid test host"))?;
        if url.scheme() != "http"
            || !matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
        {
            return Err(AppError::internal("test provider must be loopback HTTP"));
        }
        let base = format!("{}/", base.trim_end_matches('/'));
        Self::build(config, &base, &base)
    }
    pub async fn exchange_code(&self, code: &str) -> Result<OAuthTokens, AppError> {
        if code.is_empty() || code.len() > 4096 {
            return Err(reauth());
        }
        self.token_request(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", self.config.redirect_uri.as_deref().unwrap()),
        ])
        .await
    }
    pub async fn refresh(&self, refresh_token: &SecretValue) -> Result<OAuthTokens, AppError> {
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
    async fn token_request(&self, params: &[(&str, &str)]) -> Result<OAuthTokens, AppError> {
        let mut form = params.to_vec();
        form.push(("client_id", self.config.client_id.as_deref().unwrap()));
        form.push((
            "client_secret",
            self.config.client_secret.as_ref().unwrap().expose(),
        ));
        let response = self
            .client
            .post(self.accounts.join("oauth/v2/token").expect("fixed path"))
            .form(&form)
            .send()
            .await
            .map_err(|_| AppError::dependency_unavailable("Zoho token endpoint is unavailable"))?;
        if response.status().as_u16() == 400 {
            return Err(reauth());
        }
        let value = bounded_json(response).await?;
        if value.get("error").is_some() {
            return Err(reauth());
        }
        let access_token = secret_field(&value, "access_token")?;
        let refresh_token = value["refresh_token"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(SecretValue::new);
        let seconds = number(&value["expires_in"])
            .filter(|n| *n > 0 && *n <= 86400)
            .ok_or_else(|| {
                AppError::dependency_unavailable("Zoho returned an invalid token expiry")
            })?;
        Ok(OAuthTokens {
            access_token,
            refresh_token,
            expires_at: Utc::now() + Duration::seconds(seconds.min(3600) as i64),
        })
    }
    pub async fn list_lists(&self, access: &SecretValue) -> Result<Vec<CampaignList>, AppError> {
        tokio::time::timeout(std::time::Duration::from_secs(30), self.list_pages(access))
            .await
            .map_err(|_| {
                AppError::dependency_unavailable(
                    "Zoho mailing-list verification exceeded its time limit",
                )
            })?
    }
    async fn list_pages(&self, access: &SecretValue) -> Result<Vec<CampaignList>, AppError> {
        let mut result = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for page in 0..100 {
            let start = (page * 100 + 1).to_string();
            let value = self
                .get(
                    "getmailinglists",
                    &[("sort", "desc"), ("fromindex", &start), ("range", "100")],
                    access,
                )
                .await?;
            if code(&value) == Some("2401".into()) {
                return Ok(result);
            }
            require_success(&value, &["0"])?;
            let rows = value["list_of_details"]
                .as_array()
                .filter(|rows| rows.len() <= 100)
                .ok_or_else(|| {
                    AppError::dependency_unavailable("Zoho returned an invalid mailing-list page")
                })?;
            let previous = result.len();
            for row in rows {
                let key = row["listkey"]
                    .as_str()
                    .filter(|v| !v.is_empty() && v.len() <= 512)
                    .ok_or_else(|| {
                        AppError::dependency_unavailable("Zoho mailing-list key is missing")
                    })?;
                if seen.insert(key.to_owned()) {
                    result.push(CampaignList {
                        key: key.into(),
                        name: row["listname"]
                            .as_str()
                            .filter(|v| v.len() <= 1000)
                            .unwrap_or("Mailing list")
                            .into(),
                        contacts: number(&row["noofcontacts"]).ok_or_else(|| {
                            AppError::dependency_unavailable(
                                "Zoho returned an invalid contact count",
                            )
                        })?,
                    });
                }
            }
            if rows.len() < 100 {
                return Ok(result);
            }
            if result.len() == previous {
                return Err(AppError::dependency_unavailable(
                    "Zoho mailing-list pagination did not advance",
                ));
            }
        }
        Err(AppError::dependency_unavailable(
            "Zoho mailing-list verification exceeded its page limit",
        ))
    }
    pub async fn recent_campaigns(
        &self,
        access: &SecretValue,
    ) -> Result<Vec<NativeCampaign>, AppError> {
        let value = self
            .get(
                "recentcampaigns",
                &[
                    ("status", "all"),
                    ("sort", "desc"),
                    ("fromindex", "1"),
                    ("range", "50"),
                ],
                access,
            )
            .await?;
        if code(&value) == Some("6101".into()) {
            return Ok(vec![]);
        }
        require_success(&value, &["0"])?;
        let rows = value["recent_campaigns"].as_array().ok_or_else(|| {
            AppError::dependency_unavailable("Zoho returned an invalid campaign response")
        })?;
        if rows.len() > 50 {
            return Err(AppError::dependency_unavailable(
                "Zoho campaign response exceeded its limit",
            ));
        }
        rows.iter()
            .map(|row| {
                Ok(NativeCampaign {
                    key: row["campaign_key"]
                        .as_str()
                        .filter(|v| !v.is_empty() && v.len() <= 512)
                        .ok_or_else(|| {
                            AppError::dependency_unavailable("Zoho campaign key is missing")
                        })?
                        .into(),
                    name: row["campaign_name"].as_str().unwrap_or("Campaign").into(),
                    status: row["campaign_status"].as_str().unwrap_or("Unknown").into(),
                })
            })
            .collect()
    }
    pub async fn get(
        &self,
        path: &str,
        params: &[(&str, &str)],
        access: &SecretValue,
    ) -> Result<Value, AppError> {
        if !matches!(
            path,
            "getmailinglists"
                | "recentcampaigns"
                | "getlistsubscribers"
                | "getcampaigndetails"
                | "campaignreports"
                | "getcampaignrecipientsdata"
        ) {
            return Err(AppError::validation(
                "unsupported Campaigns API operation",
                vec![],
            ));
        }
        let mut url = self
            .api
            .join(&format!("api/v1.1/{path}"))
            .expect("fixed API path");
        url.query_pairs_mut().append_pair("resfmt", "JSON");
        for (key, value) in params {
            url.query_pairs_mut().append_pair(key, value);
        }
        let mut authorization =
            reqwest::header::HeaderValue::from_str(&format!("Zoho-oauthtoken {}", access.expose()))
                .map_err(|_| AppError::dependency_unavailable("Zoho token is invalid"))?;
        authorization.set_sensitive(true);
        let response = self
            .client
            .get(url)
            .header(reqwest::header::AUTHORIZATION, authorization)
            .send()
            .await
            .map_err(|_| AppError::dependency_unavailable("Zoho Campaigns is unavailable"))?;
        bounded_json(response).await
    }
}

pub fn number(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}
pub fn code(value: &Value) -> Option<String> {
    value.get("code").and_then(|v| {
        v.as_str()
            .map(str::to_owned)
            .or_else(|| v.as_i64().map(|n| n.to_string()))
    })
}
pub fn require_success(value: &Value, allowed: &[&str]) -> Result<(), AppError> {
    if code(value).is_some_and(|c| ["401", "1007", "901", "902"].contains(&c.as_str())) {
        return Err(reauth());
    }
    if code(value).is_some_and(|c| allowed.contains(&c.as_str())) {
        return Ok(());
    }
    Err(AppError::dependency_unavailable(
        "Zoho Campaigns rejected the operation; verify account permissions and provider review status",
    ))
}
fn reauth() -> AppError {
    AppError::new(
        ErrorCode::IntegrationReauthRequired,
        "Zoho authorization failed; verify the OAuth client, redirect URI and consent",
    )
}
fn secret_field(value: &Value, field: &str) -> Result<SecretValue, AppError> {
    value[field]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 16384 && !s.chars().any(char::is_whitespace))
        .map(SecretValue::new)
        .ok_or_else(|| AppError::dependency_unavailable("Zoho returned invalid credentials"))
}
async fn bounded_json(mut response: Response) -> Result<Value, AppError> {
    if matches!(response.status().as_u16(), 401 | 403) {
        return Err(reauth());
    }
    if response.status().as_u16() == 429 {
        let seconds = response
            .headers()
            .get("retry-after")
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(60);
        return Err(
            AppError::new(ErrorCode::RateLimited, "Zoho Campaigns rate limit reached")
                .with_retry_after(seconds),
        );
    }
    if !response.status().is_success() {
        return Err(AppError::dependency_unavailable(
            "Zoho returned an unsuccessful HTTP response",
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| AppError::dependency_unavailable("Zoho response was interrupted"))?
    {
        if bytes.len() + chunk.len() > 1024 * 1024 {
            return Err(AppError::dependency_unavailable(
                "Zoho response exceeded the allowed size",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| AppError::dependency_unavailable("Zoho returned invalid JSON"))?;
    Ok(value
        .get("response")
        .filter(|v| v.is_object())
        .cloned()
        .unwrap_or(value))
}
