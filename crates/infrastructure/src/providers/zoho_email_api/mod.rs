//! Zoho Campaigns Email API v2 client.
//!
//! This API-key client is intentionally separate from Campaigns OAuth and
//! Zoho Mail. It sends already-rendered content as supplied by the caller;
//! it does not add unsubscribe markup, headers, scheduling, or retries.

use super::{DeliveryCapabilities, SendOutcome, SendRequest, zoho_campaigns::SecretValue};
use reqwest::{Client, Method, Response, Url, header::HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::time::Duration;
use thiserror::Error;

const API_BASE: &str = "https://campaigns.zoho.com/emailapi/v2/";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_JSON_BYTES: usize = 1024 * 1024;
const MAX_TEMPLATE_CONTENT_BYTES: usize = 512 * 1024;
const MAX_TRANSMISSION_CONTENT_BYTES: usize = 512 * 1024;
const MAX_SUPPRESSION_PAGE_SIZE: u32 = 500;

/// Safe, provider-independent errors. Display and Debug never include
/// credentials, addresses, message content, or provider response payloads.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum ZohoEmailApiError {
    #[error("Zoho Email API is not configured")]
    NotConfigured,
    #[error("Zoho Email API client configuration is invalid")]
    InvalidConfiguration,
    #[error("invalid Zoho Email API request")]
    InvalidRequest,
    #[error("Zoho Email API does not support this request feature")]
    UnsupportedFeature,
    #[error("Zoho Email API request could not be sent")]
    Transport,
    #[error("Zoho Email API response is invalid")]
    InvalidResponse,
    #[error("Zoho Email API request exceeds the allowed size")]
    RequestTooLarge,
    #[error("Zoho Email API response exceeds the allowed size")]
    ResponseTooLarge,
    #[error("Zoho Email API returned HTTP {status}")]
    HttpStatus { status: u16 },
    #[error("Zoho Email API rate limited the request")]
    RateLimited { retry_after_seconds: Option<u32> },
    #[error("Zoho Email API rejected the request with code {code:?}")]
    Rejected { code: Option<u32> },
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TemplateContentType {
    Html,
    Text,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateSummary {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateDetails {
    pub id: String,
    pub name: String,
    pub content_type: TemplateContentType,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateWriteRequest {
    pub name: String,
    pub content: String,
    pub content_type: TemplateContentType,
}

/// Read-only transmission metadata returned by Zoho's documented detail API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransmissionDetails {
    pub transmission_id: String,
    pub subject: String,
    pub from_address: String,
    pub reply_to: Option<String>,
    pub transmission_name: String,
    pub status: String,
    pub sent_time_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SuppressionEntry {
    pub address: String,
    pub additional_data: Option<Map<String, Value>>,
    pub reason: Option<Map<String, Value>>,
    pub added_time_ms: Option<i64>,
    pub modified_time_ms: Option<i64>,
    pub suppression_type: Option<String>,
    /// Zoho documents -1 for entries added automatically by its systems.
    pub zuid: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SuppressionRange {
    pub start_index: u32,
    pub end_index: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SuppressionPage {
    pub entries: Vec<SuppressionEntry>,
    pub range: SuppressionRange,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SuppressionAddRequest {
    pub address: String,
    pub additional_data: Option<Map<String, Value>>,
    pub reason: Option<Map<String, Value>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SuppressionAddResult {
    pub added: Vec<String>,
    pub already_existing: Vec<String>,
}

#[derive(Clone)]
pub struct ZohoEmailApi {
    client: Client,
    api_base: Url,
    api_key: Option<SecretValue>,
}

impl std::fmt::Debug for ZohoEmailApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZohoEmailApi")
            .field("api_base", &self.api_base)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

impl ZohoEmailApi {
    /// Build a client for the fixed US Zoho Email API v2 endpoint.
    /// An absent API key is reported as NotConfigured when an operation is
    /// attempted so application capability/config gates can inspect a client
    /// before deciding whether to call it.
    pub fn new(api_key: Option<SecretValue>) -> Result<Self, ZohoEmailApiError> {
        Self::build(api_key, API_BASE, REQUEST_TIMEOUT)
    }

    fn build(
        api_key: Option<SecretValue>,
        api_base: &str,
        timeout: Duration,
    ) -> Result<Self, ZohoEmailApiError> {
        if timeout.is_zero() {
            return Err(ZohoEmailApiError::InvalidConfiguration);
        }
        let api_base = Url::parse(api_base).map_err(|_| ZohoEmailApiError::InvalidConfiguration)?;
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(timeout)
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(|_| ZohoEmailApiError::InvalidConfiguration)?;
        Ok(Self {
            client,
            api_base,
            api_key,
        })
    }

    /// Test-only constructor. It accepts only a loopback HTTP origin and uses
    /// the same API paths, auth header, body limits, timeouts, and redirect
    /// policy as the production client.
    #[cfg(feature = "testing")]
    pub fn for_test(
        api_key: Option<SecretValue>,
        base_url: &str,
    ) -> Result<Self, ZohoEmailApiError> {
        Self::for_test_with_timeout(api_key, base_url, REQUEST_TIMEOUT)
    }

    /// Test-only constructor with a shorter deadline for deterministic timeout
    /// coverage.
    #[cfg(feature = "testing")]
    pub fn for_test_with_timeout(
        api_key: Option<SecretValue>,
        base_url: &str,
        timeout: Duration,
    ) -> Result<Self, ZohoEmailApiError> {
        let origin = Url::parse(base_url).map_err(|_| ZohoEmailApiError::InvalidConfiguration)?;
        if origin.scheme() != "http"
            || !matches!(origin.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.query().is_some()
            || origin.fragment().is_some()
        {
            return Err(ZohoEmailApiError::InvalidConfiguration);
        }
        let api_base = format!("{}/emailapi/v2/", base_url.trim_end_matches('/'));
        Self::build(api_key, &api_base, timeout)
    }

    /// Zoho Email API supports inline delivery, but does not document custom
    /// headers or request idempotency. Office owns the unsubscribe and
    /// booking links embedded in the message body.
    pub fn capabilities(&self) -> DeliveryCapabilities {
        DeliveryCapabilities {
            supports_send: self.authorization_header().is_ok(),
            one_click_unsubscribe: false,
            custom_headers: false,
            idempotency: false,
        }
    }

    pub async fn list_templates(
        &self,
        start_index: u32,
        end_index: u32,
    ) -> Result<Vec<TemplateSummary>, ZohoEmailApiError> {
        if start_index == 0
            || end_index < start_index
            || end_index.saturating_sub(start_index) >= 1000
        {
            return Err(ZohoEmailApiError::InvalidRequest);
        }
        let mut url = self.endpoint(&["templates"]);
        url.query_pairs_mut()
            .append_pair("start_index", &start_index.to_string())
            .append_pair("end_index", &end_index.to_string());
        let value = self.request_json(Method::GET, url, None).await?;
        require_code(&value, 200701)?;
        let rows = value
            .get("templates")
            .and_then(Value::as_array)
            .ok_or(ZohoEmailApiError::InvalidResponse)?;
        rows.iter()
            .map(|row| {
                Ok(TemplateSummary {
                    id: string_field(row, "id")?,
                    name: string_field(row, "name")?,
                })
            })
            .collect()
    }

    pub async fn get_template(
        &self,
        template_id: &str,
    ) -> Result<TemplateDetails, ZohoEmailApiError> {
        validate_id(template_id)?;
        let url = self.endpoint(&["templates", template_id]);
        let value = self.request_json(Method::GET, url, None).await?;
        require_code(&value, 200702)?;
        let details = value
            .get("template_details")
            .ok_or(ZohoEmailApiError::InvalidResponse)?;
        Ok(TemplateDetails {
            id: string_field(details, "id")?,
            name: string_field(details, "name")?,
            content_type: serde_json::from_value(
                details
                    .get("content_type")
                    .cloned()
                    .ok_or(ZohoEmailApiError::InvalidResponse)?,
            )
            .map_err(|_| ZohoEmailApiError::InvalidResponse)?,
            content: string_field(details, "content")?,
        })
    }

    pub async fn create_template(
        &self,
        request: TemplateWriteRequest,
    ) -> Result<String, ZohoEmailApiError> {
        validate_template(&request)?;
        let body = serde_json::to_vec(&TemplatePayload::from(&request))
            .map_err(|_| ZohoEmailApiError::InvalidRequest)?;
        let url = self.endpoint(&["templates"]);
        let value = self.request_json(Method::POST, url, Some(body)).await?;
        require_code(&value, 200703)?;
        string_field(&value, "template_id")
    }

    pub async fn update_template(
        &self,
        template_id: &str,
        request: TemplateWriteRequest,
    ) -> Result<(), ZohoEmailApiError> {
        validate_id(template_id)?;
        validate_template(&request)?;
        let body = serde_json::to_vec(&TemplatePayload::from(&request))
            .map_err(|_| ZohoEmailApiError::InvalidRequest)?;
        let url = self.endpoint(&["templates", template_id]);
        let value = self.request_json(Method::PUT, url, Some(body)).await?;
        require_code(&value, 200704)
    }

    pub async fn delete_template(&self, template_id: &str) -> Result<(), ZohoEmailApiError> {
        validate_id(template_id)?;
        let url = self.endpoint(&["templates", template_id]);
        let value = self.request_json(Method::DELETE, url, None).await?;
        require_code(&value, 200705)
    }

    /// Send one recipient with the body rendered by the Office editor.
    ///
    /// Zoho's API has no documented custom header, reply threading, or
    /// idempotency support. Unsupported SendRequest fields are rejected
    /// locally. The correlation id is sent only as Zoho's transmission name;
    /// Zoho does not document it as an idempotency key.
    pub async fn send_inline(
        &self,
        request: SendRequest,
        sender_display_name: Option<&str>,
        reply_to: Option<&str>,
    ) -> Result<SendOutcome, ZohoEmailApiError> {
        let payload = TransmissionPayload::try_from((request, sender_display_name, reply_to))?;
        let body = serde_json::to_vec(&payload).map_err(|_| ZohoEmailApiError::InvalidRequest)?;
        if body.len() > MAX_JSON_BYTES {
            return Err(ZohoEmailApiError::RequestTooLarge);
        }
        let url = self.endpoint(&["transmission"]);
        let response = match self.request(Method::POST, url, Some(body)).await {
            Ok(response) => response,
            Err(ZohoEmailApiError::Transport) => {
                return Ok(unknown("Zoho Email API transmission result is ambiguous"));
            }
            Err(error) => return Err(error),
        };
        let status = response.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Ok(SendOutcome::Rejected {
                permanent: false,
                code: "rate_limited".into(),
            });
        }
        if status.is_server_error()
            || status.is_redirection()
            || status == reqwest::StatusCode::REQUEST_TIMEOUT
        {
            return Ok(unknown("Zoho Email API transmission result is ambiguous"));
        }
        if status.is_client_error() {
            return Ok(SendOutcome::Rejected {
                permanent: true,
                code: format!("http_{}", status.as_u16()),
            });
        }
        if !status.is_success() {
            return Ok(unknown("Zoho Email API transmission result is ambiguous"));
        }
        let bytes = match read_bounded(response).await {
            Ok(bytes) => bytes,
            Err(_) => return Ok(unknown("Zoho Email API transmission result is ambiguous")),
        };
        let value: Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) => {
                return Ok(unknown(
                    "Zoho Email API returned an invalid transmission response",
                ));
            }
        };
        let code = response_code(&value);
        if code != Some(200001) {
            if let Some(code) = error_code(&value).or(code) {
                return Ok(SendOutcome::Rejected {
                    permanent: true,
                    code: format!("api_{code}"),
                });
            }
            return Ok(unknown("Zoho Email API transmission result is ambiguous"));
        }
        let accepted = value.get("accepted_recipients").and_then(Value::as_u64);
        let rejected = value.get("rejected_recipients").and_then(Value::as_u64);
        match (accepted, rejected) {
            (Some(1), Some(0)) => {
                let Some(transmission_id) = value
                    .get("transmission_id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                else {
                    return Ok(unknown("Zoho Email API omitted the transmission id"));
                };
                Ok(SendOutcome::Accepted {
                    provider_message_id: transmission_id.to_owned(),
                })
            }
            (Some(0), Some(1)) => Ok(SendOutcome::Rejected {
                permanent: false,
                code: error_code(&value)
                    .map(|code| format!("api_{code}"))
                    .unwrap_or_else(|| "recipient_rejected".into()),
            }),
            _ => Ok(unknown(
                "Zoho Email API returned ambiguous recipient counts",
            )),
        }
    }

    /// Read transmission details by a provider-issued transmission id. This
    /// cannot look up by correlation id or prove provider-side idempotency.
    pub async fn get_transmission(
        &self,
        transmission_id: &str,
    ) -> Result<TransmissionDetails, ZohoEmailApiError> {
        validate_id(transmission_id)?;
        let url = self.endpoint(&["transmission", transmission_id]);
        let value = self.request_json(Method::GET, url, None).await?;
        require_code(&value, 200004)?;
        let details = value
            .get("transmission_details")
            .ok_or(ZohoEmailApiError::InvalidResponse)?;
        Ok(TransmissionDetails {
            transmission_id: transmission_id.to_owned(),
            subject: string_field(details, "subject")?,
            from_address: string_field(details, "from")?,
            reply_to: optional_string_field(details, "reply_to")?,
            transmission_name: string_field(details, "transmission_name")?,
            status: string_field(details, "status")?,
            sent_time_ms: details.get("sent_time").and_then(Value::as_i64),
        })
    }

    /// Read one page of Zoho's suppression list. The explicit 500-entry page
    /// limit matches Zoho's documented example and bounds each page; callers
    /// can request later pages with their returned range.
    pub async fn get_suppressions(
        &self,
        start_index: u32,
        end_index: u32,
    ) -> Result<SuppressionPage, ZohoEmailApiError> {
        if start_index == 0
            || end_index < start_index
            || end_index.saturating_sub(start_index) >= MAX_SUPPRESSION_PAGE_SIZE
        {
            return Err(ZohoEmailApiError::InvalidRequest);
        }
        let mut url = self.endpoint(&["recipients", "suppression"]);
        url.query_pairs_mut()
            .append_pair("start_index", &start_index.to_string())
            .append_pair("end_index", &end_index.to_string());
        let value = self.request_json(Method::GET, url, None).await?;
        require_code(&value, 200401)?;
        let rows = value
            .get("suppression")
            .and_then(Value::as_array)
            .ok_or(ZohoEmailApiError::InvalidResponse)?;
        let entries = rows
            .iter()
            .map(parse_suppression_entry)
            .collect::<Result<Vec<_>, _>>()?;
        let range = value
            .get("range")
            .ok_or(ZohoEmailApiError::InvalidResponse)?;
        Ok(SuppressionPage {
            entries,
            range: SuppressionRange {
                start_index: u32_field(range, "start_index")?,
                end_index: u32_field(range, "end_index")?,
            },
        })
    }

    /// Add recipients to Zoho's suppression list. This API deliberately has
    /// no matching removal operation: it cannot undo an opt-out or remove a
    /// system-added suppression.
    pub async fn add_suppressions(
        &self,
        recipients: &[SuppressionAddRequest],
    ) -> Result<SuppressionAddResult, ZohoEmailApiError> {
        if recipients.is_empty() || recipients.iter().any(|r| r.address.trim().is_empty()) {
            return Err(ZohoEmailApiError::InvalidRequest);
        }
        let payload = SuppressionAddPayload {
            recipients: recipients
                .iter()
                .map(|recipient| SuppressionRecipientPayload {
                    address: &recipient.address,
                    additional_data: recipient.additional_data.as_ref(),
                    reason: recipient.reason.as_ref(),
                })
                .collect(),
        };
        let body = serde_json::to_vec(&payload).map_err(|_| ZohoEmailApiError::InvalidRequest)?;
        let url = self.endpoint(&["recipients", "suppression"]);
        let value = self.request_json(Method::POST, url, Some(body)).await?;
        require_code(&value, 200402)?;
        Ok(SuppressionAddResult {
            added: string_array_field(&value, "suppressions_added")?,
            already_existing: string_array_field(&value, "suppressions_already_existing")?,
        })
    }

    fn endpoint(&self, segments: &[&str]) -> Url {
        let mut url = self.api_base.clone();
        {
            let mut path = url
                .path_segments_mut()
                .expect("the configured API base is hierarchical");
            path.pop_if_empty();
            for segment in segments {
                path.push(segment);
            }
        }
        url
    }

    async fn request_json(
        &self,
        method: Method,
        url: Url,
        body: Option<Vec<u8>>,
    ) -> Result<Value, ZohoEmailApiError> {
        let response = self.request(method, url, body).await?;
        let status = response.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after_seconds = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u32>().ok());
            return Err(ZohoEmailApiError::RateLimited {
                retry_after_seconds,
            });
        }
        if !status.is_success() {
            return Err(ZohoEmailApiError::HttpStatus {
                status: status.as_u16(),
            });
        }
        let bytes = read_bounded(response).await?;
        serde_json::from_slice(&bytes).map_err(|_| ZohoEmailApiError::InvalidResponse)
    }

    async fn request(
        &self,
        method: Method,
        url: Url,
        body: Option<Vec<u8>>,
    ) -> Result<Response, ZohoEmailApiError> {
        let auth = self.authorization_header()?;
        let mut request = self
            .client
            .request(method, url)
            .header(reqwest::header::AUTHORIZATION, auth)
            .header(reqwest::header::ACCEPT, "application/json");
        if let Some(body) = body {
            if body.len() > MAX_JSON_BYTES {
                return Err(ZohoEmailApiError::RequestTooLarge);
            }
            request = request
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body);
        }
        request
            .send()
            .await
            .map_err(|_| ZohoEmailApiError::Transport)
    }

    fn authorization_header(&self) -> Result<HeaderValue, ZohoEmailApiError> {
        let key = self
            .api_key
            .as_ref()
            .map(SecretValue::expose)
            .filter(|key| !key.trim().is_empty())
            .ok_or(ZohoEmailApiError::NotConfigured)?;
        HeaderValue::from_str(&format!("Zoho-zapikey {key}"))
            .map_err(|_| ZohoEmailApiError::InvalidConfiguration)
    }
}

#[derive(Serialize)]
struct TemplatePayload<'a> {
    template_name: &'a str,
    content: &'a str,
    content_type: TemplateContentType,
}

impl<'a> From<&'a TemplateWriteRequest> for TemplatePayload<'a> {
    fn from(value: &'a TemplateWriteRequest) -> Self {
        Self {
            template_name: &value.name,
            content: &value.content,
            content_type: value.content_type,
        }
    }
}

fn validate_template(request: &TemplateWriteRequest) -> Result<(), ZohoEmailApiError> {
    if request.name.trim().is_empty() || request.content.is_empty() {
        return Err(ZohoEmailApiError::InvalidRequest);
    }
    if request.content.len() > MAX_TEMPLATE_CONTENT_BYTES {
        return Err(ZohoEmailApiError::RequestTooLarge);
    }
    Ok(())
}

fn validate_id(value: &str) -> Result<(), ZohoEmailApiError> {
    if value.trim().is_empty() || value.len() > 200 {
        return Err(ZohoEmailApiError::InvalidRequest);
    }
    Ok(())
}

#[derive(Serialize)]
struct TransmissionPayload {
    transmission_name: String,
    recipients: [RecipientPayload; 1],
    options: TrackingOptions,
    content: ContentPayload,
}

#[derive(Serialize)]
struct RecipientPayload {
    address: String,
}

#[derive(Serialize)]
struct TrackingOptions {
    open_tracking: &'static str,
    click_tracking: &'static str,
}

#[derive(Serialize)]
struct ContentPayload {
    from: FromPayload,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_to: Option<String>,
    subject: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    html: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
}

#[derive(Serialize)]
struct FromPayload {
    address: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

impl TryFrom<(SendRequest, Option<&str>, Option<&str>)> for TransmissionPayload {
    type Error = ZohoEmailApiError;

    fn try_from(
        (request, sender_display_name, reply_to): (SendRequest, Option<&str>, Option<&str>),
    ) -> Result<Self, Self::Error> {
        if request.in_reply_to.is_some() || !request.headers.is_empty() {
            return Err(ZohoEmailApiError::UnsupportedFeature);
        }
        if request.correlation_id.trim().is_empty()
            || request.correlation_id.chars().count() > 150
            || request.from.trim().is_empty()
            || request.from.chars().count() > 100
            || request.to.trim().is_empty()
            || request.to.chars().count() > 100
            || request.subject.trim().is_empty()
            || request.subject.chars().count() > 200
            || sender_display_name
                .is_some_and(|name| name.trim().is_empty() || name.chars().count() > 100)
            || reply_to
                .is_some_and(|address| address.trim().is_empty() || address.chars().count() > 100)
        {
            return Err(ZohoEmailApiError::InvalidRequest);
        }
        let has_content = request
            .body_html
            .as_deref()
            .is_some_and(|body| !body.is_empty())
            || request
                .body_text
                .as_deref()
                .is_some_and(|body| !body.is_empty());
        if !has_content {
            return Err(ZohoEmailApiError::InvalidRequest);
        }
        if request
            .body_html
            .as_ref()
            .is_some_and(|body| body.len() > MAX_TRANSMISSION_CONTENT_BYTES)
            || request
                .body_text
                .as_ref()
                .is_some_and(|body| body.len() > MAX_TRANSMISSION_CONTENT_BYTES)
        {
            return Err(ZohoEmailApiError::RequestTooLarge);
        }
        Ok(Self {
            transmission_name: request.correlation_id,
            recipients: [RecipientPayload {
                address: request.to,
            }],
            options: TrackingOptions {
                open_tracking: "disabled",
                click_tracking: "disabled",
            },
            content: ContentPayload {
                from: FromPayload {
                    address: request.from,
                    name: sender_display_name.map(str::to_owned),
                },
                reply_to: reply_to.map(str::to_owned),
                subject: request.subject,
                html: request.body_html,
                text: request.body_text,
            },
        })
    }
}

fn unknown(reason: &str) -> SendOutcome {
    SendOutcome::Unknown {
        reason: reason.to_owned(),
    }
}

async fn read_bounded(mut response: Response) -> Result<Vec<u8>, ZohoEmailApiError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_JSON_BYTES as u64)
    {
        return Err(ZohoEmailApiError::ResponseTooLarge);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| ZohoEmailApiError::Transport)?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_JSON_BYTES {
            return Err(ZohoEmailApiError::ResponseTooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn require_code(value: &Value, expected: u32) -> Result<(), ZohoEmailApiError> {
    let code = response_code(value).or_else(|| error_code(value));
    if code == Some(expected) {
        Ok(())
    } else if code.is_some() || value.get("errors").is_some() {
        Err(ZohoEmailApiError::Rejected { code })
    } else {
        Err(ZohoEmailApiError::InvalidResponse)
    }
}

fn response_code(value: &Value) -> Option<u32> {
    value
        .get("response")
        .and_then(|response| response.get("code"))
        .and_then(number_code)
}

fn error_code(value: &Value) -> Option<u32> {
    let errors = value.get("errors")?;
    let first = errors
        .as_array()
        .and_then(|values| values.first())
        .or_else(|| errors.is_object().then_some(errors))?;
    first.get("code").and_then(number_code)
}

fn number_code(value: &Value) -> Option<u32> {
    value
        .as_u64()
        .and_then(|number| u32::try_from(number).ok())
        .or_else(|| value.as_str().and_then(|value| value.parse::<u32>().ok()))
}

fn string_field(value: &Value, field: &str) -> Result<String, ZohoEmailApiError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|value| !value.is_empty())
        .ok_or(ZohoEmailApiError::InvalidResponse)
}

fn optional_string_field(value: &Value, field: &str) -> Result<Option<String>, ZohoEmailApiError> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value.clone())),
        _ => Err(ZohoEmailApiError::InvalidResponse),
    }
}

#[derive(Serialize)]
struct SuppressionAddPayload<'a> {
    recipients: Vec<SuppressionRecipientPayload<'a>>,
}

#[derive(Serialize)]
struct SuppressionRecipientPayload<'a> {
    address: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    additional_data: Option<&'a Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'a Map<String, Value>>,
}

fn parse_suppression_entry(value: &Value) -> Result<SuppressionEntry, ZohoEmailApiError> {
    Ok(SuppressionEntry {
        address: string_field(value, "address")?,
        additional_data: optional_object_field(value, "additional_data")?,
        reason: optional_object_field(value, "reason")?,
        added_time_ms: optional_i64_field(value, "added_time")?,
        modified_time_ms: optional_i64_field(value, "modified_time")?,
        suppression_type: optional_string_field(value, "suppression_type")?,
        zuid: optional_i64_field(value, "zuid")?,
    })
}

fn optional_object_field(
    value: &Value,
    field: &str,
) -> Result<Option<Map<String, Value>>, ZohoEmailApiError> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(value)) => Ok(Some(value.clone())),
        _ => Err(ZohoEmailApiError::InvalidResponse),
    }
}

fn optional_i64_field(value: &Value, field: &str) -> Result<Option<i64>, ZohoEmailApiError> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(value)) => value
            .as_i64()
            .map(Some)
            .ok_or(ZohoEmailApiError::InvalidResponse),
        Some(Value::String(value)) => value
            .parse::<i64>()
            .map(Some)
            .map_err(|_| ZohoEmailApiError::InvalidResponse),
        _ => Err(ZohoEmailApiError::InvalidResponse),
    }
}

fn u32_field(value: &Value, field: &str) -> Result<u32, ZohoEmailApiError> {
    let number = value.get(field).ok_or(ZohoEmailApiError::InvalidResponse)?;
    number
        .as_u64()
        .and_then(|number| u32::try_from(number).ok())
        .or_else(|| {
            number
                .as_str()
                .and_then(|number| number.parse::<u32>().ok())
        })
        .ok_or(ZohoEmailApiError::InvalidResponse)
}

fn string_array_field(value: &Value, field: &str) -> Result<Vec<String>, ZohoEmailApiError> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
                    .ok_or(ZohoEmailApiError::InvalidResponse)
            })
            .collect(),
        _ => Err(ZohoEmailApiError::InvalidResponse),
    }
}
