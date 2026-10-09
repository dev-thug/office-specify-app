//! Bounded HTTP client for the US Zoho Mail REST API.
//!
//! The client only builds routes from documented, fixed paths and numeric
//! Zoho IDs. Production requests use `https://mail.zoho.com`; the test-only
//! constructor accepts loopback HTTP origins. JSON responses are capped at
//! 1 MiB, redirects are disabled, and sends are never retried.

use super::{SendOutcome, zoho_campaigns::SecretValue};
use reqwest::{Client, Method, RequestBuilder, Response, StatusCode, Url, header};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
#[cfg(feature = "testing")]
use std::net::IpAddr;
use std::{fmt, time::Duration};

const API_BASE: &str = "https://mail.zoho.com/";
const API_PATH: &str = "api/";
const MAX_JSON_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_ATTACHMENT_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(feature = "testing")]
const TEST_REQUEST_TIMEOUT: Duration = Duration::from_millis(100);
#[cfg(feature = "testing")]
const TEST_CONNECT_TIMEOUT: Duration = Duration::from_millis(100);

/// Zoho Mail API client bound to one access token and the US Mail API host.
///
/// Create a new client after refreshing OAuth credentials so calls always use
/// the current access token. The token is never included in this type's Debug
/// output or any client error.
pub struct ZohoMailClient {
    http: Client,
    base_url: Url,
    access_token: SecretValue,
}

impl fmt::Debug for ZohoMailClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ZohoMailClient")
            .field("base_url", &self.base_url)
            .field("access_token", &"[REDACTED]")
            .finish()
    }
}

impl ZohoMailClient {
    /// Build a production client fixed to `https://mail.zoho.com`.
    pub fn new(access_token: SecretValue) -> Result<Self, ZohoMailClientError> {
        Self::build(
            access_token,
            Url::parse(API_BASE).expect("valid fixed Zoho API URL"),
            REQUEST_TIMEOUT,
            CONNECT_TIMEOUT,
        )
    }

    /// Build a loopback-only HTTP client for local mock-server tests.
    ///
    /// This constructor is available only with the infrastructure `testing`
    /// feature. Its URL must be plain HTTP with a loopback IP literal, without
    /// user info, query, fragment, or a non-root path.
    #[cfg(feature = "testing")]
    pub fn with_base_url_for_testing(
        access_token: SecretValue,
        base_url: &str,
    ) -> Result<Self, ZohoMailClientError> {
        let base_url = Url::parse(base_url).map_err(|_| ZohoMailClientError::UnsafeTestOrigin)?;
        let loopback_ip = base_url
            .host_str()
            .and_then(|host| host.parse::<IpAddr>().ok())
            .is_some_and(|ip| ip.is_loopback());
        if base_url.scheme() != "http"
            || !loopback_ip
            || !base_url.username().is_empty()
            || base_url.password().is_some()
            || base_url.query().is_some()
            || base_url.fragment().is_some()
            || base_url.path() != "/"
        {
            return Err(ZohoMailClientError::UnsafeTestOrigin);
        }
        Self::build(
            access_token,
            base_url,
            TEST_REQUEST_TIMEOUT,
            TEST_CONNECT_TIMEOUT,
        )
    }

    fn build(
        access_token: SecretValue,
        base_url: Url,
        timeout: Duration,
        connect_timeout: Duration,
    ) -> Result<Self, ZohoMailClientError> {
        if access_token.expose().trim().is_empty()
            || !access_token
                .expose()
                .bytes()
                .all(|byte| byte.is_ascii_graphic())
        {
            return Err(ZohoMailClientError::InvalidAccessToken);
        }
        let http = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .retry(reqwest::retry::never())
            .timeout(timeout)
            .connect_timeout(connect_timeout)
            .build()
            .map_err(|_| ZohoMailClientError::ClientInitialization)?;
        Ok(Self {
            http,
            base_url,
            access_token,
        })
    }

    /// Discover all mailbox accounts available to the authenticated user.
    pub async fn list_accounts(&self) -> Result<Vec<ZohoAccount>, ZohoMailClientError> {
        self.get_json("accounts", &[]).await
    }

    /// Retrieve details for one account.
    pub async fn get_account(
        &self,
        account_id: &str,
    ) -> Result<ZohoAccountDetails, ZohoMailClientError> {
        validate_id("account_id", account_id)?;
        self.get_json(&format!("accounts/{account_id}"), &[]).await
    }

    /// List the folders for an account.
    pub async fn list_folders(
        &self,
        account_id: &str,
    ) -> Result<Vec<ZohoFolder>, ZohoMailClientError> {
        validate_id("account_id", account_id)?;
        self.get_json(&format!("accounts/{account_id}/folders"), &[])
            .await
    }

    /// List messages in a folder. Zoho's API allows 1–200 messages per page.
    pub async fn list_messages(
        &self,
        account_id: &str,
        folder_id: &str,
        options: &ZohoMailListOptions,
    ) -> Result<Vec<ZohoMessageSummary>, ZohoMailClientError> {
        validate_id("account_id", account_id)?;
        validate_id("folder_id", folder_id)?;
        validate_page(options.start, options.limit)?;
        let mut query = vec![
            ("folderId", folder_id.to_owned()),
            ("start", options.start.to_string()),
            ("limit", options.limit.to_string()),
            ("includeto", options.include_to.to_string()),
        ];
        if let Some(status) = options.status {
            query.push(("status", status.as_api_value().to_owned()));
        }
        if let Some(sort_by) = options.sort_by {
            query.push(("sortBy", sort_by.as_api_value().to_owned()));
        }
        if let Some(sort_order) = options.sort_order {
            query.push(("sortorder", sort_order.to_string()));
        }
        if let Some(thread_id) = options.thread_id.as_deref() {
            validate_id("thread_id", thread_id)?;
            query.push(("threadId", thread_id.to_owned()));
        }
        self.get_json(&format!("accounts/{account_id}/messages/view"), &query)
            .await
    }

    /// Search mailbox messages using Zoho Mail's documented search syntax.
    pub async fn search_messages(
        &self,
        account_id: &str,
        search_key: &str,
        options: &ZohoMailSearch,
    ) -> Result<Vec<ZohoMessageSummary>, ZohoMailClientError> {
        validate_id("account_id", account_id)?;
        validate_nonempty("search_key", search_key)?;
        validate_page(options.start, options.limit)?;
        let mut query = vec![
            ("searchKey", search_key.to_owned()),
            ("start", options.start.to_string()),
            ("limit", options.limit.to_string()),
            ("includeto", options.include_to.to_string()),
        ];
        if let Some(received_time) = options.received_time {
            query.push(("receivedTime", received_time.to_string()));
        }
        self.get_json(&format!("accounts/{account_id}/messages/search"), &query)
            .await
    }

    /// Retrieve metadata for a message using its Zoho folder and message IDs.
    pub async fn message_details(
        &self,
        account_id: &str,
        folder_id: &str,
        message_id: &str,
    ) -> Result<ZohoMessageDetails, ZohoMailClientError> {
        validate_message_path(account_id, folder_id, message_id)?;
        self.get_json(
            &format!("accounts/{account_id}/folders/{folder_id}/messages/{message_id}/details"),
            &[],
        )
        .await
    }

    /// Retrieve HTML or plain text content for a message.
    pub async fn message_content(
        &self,
        account_id: &str,
        folder_id: &str,
        message_id: &str,
    ) -> Result<ZohoMailMessageContent, ZohoMailClientError> {
        validate_message_path(account_id, folder_id, message_id)?;
        self.get_json(
            &format!("accounts/{account_id}/folders/{folder_id}/messages/{message_id}/content"),
            &[],
        )
        .await
    }

    /// Retrieve attachment metadata needed to request individual attachment
    /// content. Inline attachments are excluded by default.
    pub async fn attachment_info(
        &self,
        account_id: &str,
        folder_id: &str,
        message_id: &str,
        include_inline: bool,
    ) -> Result<ZohoAttachmentInfo, ZohoMailClientError> {
        validate_message_path(account_id, folder_id, message_id)?;
        let query = [("includeInline", include_inline.to_string())];
        self.get_json(
            &format!(
                "accounts/{account_id}/folders/{folder_id}/messages/{message_id}/attachmentinfo"
            ),
            &query,
        )
        .await
    }

    /// Retrieve one attachment's bytes. Responses above 32 MiB are rejected.
    pub async fn attachment_content(
        &self,
        account_id: &str,
        folder_id: &str,
        message_id: &str,
        attachment_id: &str,
    ) -> Result<Vec<u8>, ZohoMailClientError> {
        validate_message_path(account_id, folder_id, message_id)?;
        validate_id("attachment_id", attachment_id)?;
        let url = self.url(&format!(
            "accounts/{account_id}/folders/{folder_id}/messages/{message_id}/attachments/{attachment_id}"
        ))?;
        let response = self
            .request(Method::GET, url, "application/octet-stream")?
            .send()
            .await
            .map_err(|_| ZohoMailClientError::Transport)?;
        if !response.status().is_success() {
            return Err(classify_http_error(&response));
        }
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let bytes = read_limited(response, MAX_ATTACHMENT_RESPONSE_BYTES).await?;
        if content_type.as_deref().is_some_and(is_json_media_type) {
            let raw: Value =
                serde_json::from_slice(&bytes).map_err(|_| ZohoMailClientError::InvalidResponse)?;
            check_business_status(&raw)?;
            return Err(ZohoMailClientError::InvalidResponse);
        }
        if let Ok(raw) = serde_json::from_slice::<Value>(&bytes)
            && is_zoho_status_envelope(&raw)
        {
            check_business_status(&raw)?;
            return Err(ZohoMailClientError::InvalidResponse);
        }
        Ok(bytes)
    }

    /// Submit a new email using Zoho's documented message-create endpoint.
    pub async fn send_message(
        &self,
        account_id: &str,
        message: &ZohoMailCompose,
    ) -> Result<SendOutcome, ZohoMailClientError> {
        validate_id("account_id", account_id)?;
        validate_compose(message)?;
        let path = format!("accounts/{account_id}/messages");
        self.send_payload(&path, &message.to_payload(None)).await
    }

    /// Reply to an email using the Zoho provider message ID returned by its
    /// list/search APIs. An Internet `Message-ID` is not accepted here.
    pub async fn reply_to_message(
        &self,
        account_id: &str,
        provider_message_id: &str,
        message: &ZohoMailCompose,
    ) -> Result<SendOutcome, ZohoMailClientError> {
        validate_id("account_id", account_id)?;
        validate_id("provider_message_id", provider_message_id)?;
        validate_compose(message)?;
        let path = format!("accounts/{account_id}/messages/{provider_message_id}");
        self.send_payload(&path, &message.to_payload(Some("reply")))
            .await
    }

    /// Save a composition as a draft. When replying from a draft, `in_reply_to`
    /// and `ref_header` are the RFC Message-ID header values Zoho documents for
    /// its draft endpoint. Direct sending replies still require a provider ID.
    pub async fn save_draft(
        &self,
        account_id: &str,
        message: &ZohoMailCompose,
        in_reply_to: Option<&str>,
        ref_header: Option<&str>,
    ) -> Result<ZohoMailMutation, ZohoMailClientError> {
        validate_id("account_id", account_id)?;
        validate_compose(message)?;
        if let Some(value) = in_reply_to {
            validate_header_value("in_reply_to", value)?;
        }
        if let Some(value) = ref_header {
            validate_header_value("ref_header", value)?;
        }
        let payload = message
            .to_payload(None)
            .with_mode("draft", in_reply_to, ref_header);
        self.post_mutation(&format!("accounts/{account_id}/messages"), &payload)
            .await
    }

    /// Save a composition as a Zoho Mail template.
    pub async fn save_template(
        &self,
        account_id: &str,
        message: &ZohoMailCompose,
    ) -> Result<ZohoMailMutation, ZohoMailClientError> {
        validate_id("account_id", account_id)?;
        validate_compose(message)?;
        let payload = message.to_payload(None).with_mode("template", None, None);
        self.post_mutation(&format!("accounts/{account_id}/messages"), &payload)
            .await
    }

    async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T, ZohoMailClientError> {
        let url = self.url(path)?;
        let response = self
            .request(Method::GET, url, "application/json")?
            .query(query)
            .send()
            .await
            .map_err(|_| ZohoMailClientError::Transport)?;
        self.decode_json_response(response).await
    }

    async fn post_mutation(
        &self,
        path: &str,
        payload: &ZohoMailPayload,
    ) -> Result<ZohoMailMutation, ZohoMailClientError> {
        let url = self.url(path)?;
        let response = self
            .request(Method::POST, url, "application/json")?
            .json(payload)
            .send()
            .await
            .map_err(|_| ZohoMailClientError::Transport)?;
        self.decode_json_response(response).await
    }

    async fn send_payload(
        &self,
        path: &str,
        payload: &ZohoMailPayload,
    ) -> Result<SendOutcome, ZohoMailClientError> {
        let url = self.url(path)?;
        let response = match self
            .request(Method::POST, url, "application/json")?
            .json(payload)
            .send()
            .await
        {
            Ok(response) => response,
            Err(_) => {
                return Ok(SendOutcome::Unknown {
                    reason: "transport_error".into(),
                });
            }
        };
        match self
            .decode_json_response::<ZohoMailMutation>(response)
            .await
        {
            Ok(result) => match result.message_id {
                Some(provider_message_id)
                    if validate_id("provider_message_id", &provider_message_id).is_ok() =>
                {
                    Ok(SendOutcome::Accepted {
                        provider_message_id,
                    })
                }
                Some(_) => Ok(SendOutcome::Unknown {
                    reason: "provider_response_invalid_message_id".into(),
                }),
                None => Ok(SendOutcome::Unknown {
                    reason: "provider_response_missing_message_id".into(),
                }),
            },
            Err(ZohoMailClientError::ReauthRequired) => Err(ZohoMailClientError::ReauthRequired),
            Err(ZohoMailClientError::RateLimited { .. }) => Ok(SendOutcome::Rejected {
                permanent: false,
                code: "rate_limited".into(),
            }),
            Err(ZohoMailClientError::Business { code: Some(code) }) if code >= 500 => {
                Ok(SendOutcome::Unknown {
                    reason: "provider_business_server_error".into(),
                })
            }
            Err(ZohoMailClientError::Business { code }) => Ok(SendOutcome::Rejected {
                permanent: true,
                code: code
                    .map_or_else(|| "provider_rejected".into(), |code| format!("zoho_{code}")),
            }),
            Err(ZohoMailClientError::Unavailable { .. })
            | Err(ZohoMailClientError::Transport)
            | Err(ZohoMailClientError::ResponseTooLarge)
            | Err(ZohoMailClientError::InvalidResponse) => Ok(SendOutcome::Unknown {
                reason: "provider_outcome_uncertain".into(),
            }),
            Err(ZohoMailClientError::HttpStatus { status }) if status >= 500 => {
                Ok(SendOutcome::Unknown {
                    reason: "provider_server_error".into(),
                })
            }
            Err(ZohoMailClientError::HttpStatus { status: 408 }) => Ok(SendOutcome::Unknown {
                reason: "provider_timeout".into(),
            }),
            Err(ZohoMailClientError::HttpStatus { status: 429 }) => Ok(SendOutcome::Rejected {
                permanent: false,
                code: "rate_limited".into(),
            }),
            Err(ZohoMailClientError::HttpStatus { status }) if (400..500).contains(&status) => {
                Ok(SendOutcome::Rejected {
                    permanent: true,
                    code: format!("http_{status}"),
                })
            }
            Err(error) => Err(error),
        }
    }

    async fn decode_json_response<T: DeserializeOwned>(
        &self,
        response: Response,
    ) -> Result<T, ZohoMailClientError> {
        if !response.status().is_success() {
            return Err(classify_http_error(&response));
        }
        let body = read_limited(response, MAX_JSON_RESPONSE_BYTES).await?;
        let value: Value =
            serde_json::from_slice(&body).map_err(|_| ZohoMailClientError::InvalidResponse)?;
        check_business_status(&value)?;
        let data = value
            .get("data")
            .cloned()
            .ok_or(ZohoMailClientError::InvalidResponse)?;
        serde_json::from_value(data).map_err(|_| ZohoMailClientError::InvalidResponse)
    }

    fn request(
        &self,
        method: Method,
        url: Url,
        accept: &'static str,
    ) -> Result<RequestBuilder, ZohoMailClientError> {
        let mut authorization = header::HeaderValue::from_str(&format!(
            "Zoho-oauthtoken {}",
            self.access_token.expose()
        ))
        .map_err(|_| ZohoMailClientError::InvalidAccessToken)?;
        authorization.set_sensitive(true);
        Ok(self
            .http
            .request(method, url)
            .header(header::AUTHORIZATION, authorization)
            .header(header::ACCEPT, accept))
    }

    fn url(&self, path: &str) -> Result<Url, ZohoMailClientError> {
        // Only private callsites provide paths, and all dynamic segments have
        // been restricted to numeric IDs before reaching this join.
        self.base_url
            .join(&format!("{API_PATH}{path}"))
            .map_err(|_| ZohoMailClientError::InvalidRequestUrl)
    }
}

/// Zoho account metadata. Fields that Zoho omits for secondary/external
/// accounts remain optional; account IDs are normalized to decimal strings.
#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ZohoAccount {
    #[serde(deserialize_with = "deserialize_required_id")]
    pub account_id: String,
    #[serde(default)]
    pub mailbox_address: Option<String>,
    #[serde(default)]
    pub primary_email_address: Option<String>,
    #[serde(default)]
    pub account_display_name: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// Account details endpoint returns the same documented account fields as
/// account discovery. The dedicated alias makes the API method intent clear.
pub type ZohoAccountDetails = ZohoAccount;

/// A Zoho mailbox folder.
#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ZohoFolder {
    #[serde(deserialize_with = "deserialize_required_id")]
    pub folder_id: String,
    pub folder_name: String,
    #[serde(default)]
    pub folder_type: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub imap_access: Option<bool>,
}

/// Inbox-list pagination and filters. Zoho accepts page limits from 1 to 200.
#[derive(Clone)]
pub struct ZohoMailListOptions {
    pub start: u32,
    pub limit: u16,
    pub include_to: bool,
    pub status: Option<ZohoMessageStatus>,
    pub sort_by: Option<ZohoMessageSort>,
    pub sort_order: Option<bool>,
    pub thread_id: Option<String>,
}

impl Default for ZohoMailListOptions {
    fn default() -> Self {
        Self {
            start: 1,
            limit: 10,
            include_to: true,
            status: None,
            sort_by: None,
            sort_order: None,
            thread_id: None,
        }
    }
}

/// Supported read/unread filters for Zoho's folder-message endpoint.
#[derive(Clone, Copy)]
pub enum ZohoMessageStatus {
    Read,
    Unread,
    All,
}

impl ZohoMessageStatus {
    fn as_api_value(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Unread => "unread",
            Self::All => "all",
        }
    }
}

/// Supported sort keys for Zoho's folder-message endpoint.
#[derive(Clone, Copy)]
pub enum ZohoMessageSort {
    Date,
    MessageId,
    Size,
}

impl ZohoMessageSort {
    fn as_api_value(self) -> &'static str {
        match self {
            Self::Date => "date",
            Self::MessageId => "messageId",
            Self::Size => "size",
        }
    }
}

/// Paging controls for Zoho's search endpoint.
#[derive(Clone, Copy)]
pub struct ZohoMailSearch {
    pub start: u32,
    pub limit: u16,
    pub include_to: bool,
    pub received_time: Option<u64>,
}

impl Default for ZohoMailSearch {
    fn default() -> Self {
        Self {
            start: 1,
            limit: 10,
            include_to: true,
            received_time: None,
        }
    }
}

/// A normalized message list entry. The timestamp and ID fields accept the
/// number-or-string forms Zoho uses across list and detail endpoints.
#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ZohoMessageSummary {
    #[serde(deserialize_with = "deserialize_required_id")]
    pub message_id: String,
    #[serde(default, deserialize_with = "deserialize_optional_id")]
    pub folder_id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_id")]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub from_address: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_stringish")]
    pub to_address: Option<String>,
    #[serde(default)]
    pub cc_address: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(
        default,
        alias = "receivedtime",
        deserialize_with = "deserialize_optional_stringish"
    )]
    pub received_time: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_stringish")]
    pub sent_date_in_gmt: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_stringish")]
    pub has_attachment: Option<String>,
}

/// Message details use Zoho's `/details` endpoint and include the summary
/// fields returned by its list endpoint.
pub type ZohoMessageDetails = ZohoMessageSummary;

/// The message body returned by Zoho's `/content` endpoint.
#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ZohoMailMessageContent {
    #[serde(default, deserialize_with = "deserialize_optional_id")]
    pub message_id: Option<String>,
    pub content: String,
}

/// Compose body format supported by Zoho Mail's send/draft endpoints.
#[derive(Clone, Copy, Default, Serialize, PartialEq)]
pub enum ZohoMailFormat {
    #[default]
    #[serde(rename = "html")]
    Html,
    #[serde(rename = "plaintext")]
    PlainText,
}

/// Inputs shared by send, reply, draft, and template operations.
///
/// The fields contain message content and addresses. This type intentionally
/// does not implement `Debug` so accidental diagnostic formatting cannot
/// expose them.
#[derive(Clone, Serialize)]
pub struct ZohoMailCompose {
    pub from_address: String,
    pub to_address: String,
    pub cc_address: Option<String>,
    pub bcc_address: Option<String>,
    pub subject: String,
    pub content: String,
    pub mail_format: ZohoMailFormat,
    pub ask_receipt: Option<bool>,
}

impl ZohoMailCompose {
    fn to_payload(&self, action: Option<&str>) -> ZohoMailPayload {
        ZohoMailPayload {
            from_address: self.from_address.clone(),
            to_address: self.to_address.clone(),
            cc_address: self.cc_address.clone(),
            bcc_address: self.bcc_address.clone(),
            subject: self.subject.clone(),
            content: self.content.clone(),
            mail_format: self.mail_format,
            ask_receipt: self
                .ask_receipt
                .map(|yes| if yes { "yes" } else { "no" }.to_owned()),
            action: action.map(str::to_owned),
            mode: None,
            in_reply_to: None,
            ref_header: None,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ZohoMailPayload {
    from_address: String,
    to_address: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cc_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bcc_address: Option<String>,
    subject: String,
    content: String,
    mail_format: ZohoMailFormat,
    #[serde(skip_serializing_if = "Option::is_none")]
    ask_receipt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    in_reply_to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ref_header: Option<String>,
}

impl ZohoMailPayload {
    fn with_mode(
        mut self,
        mode: &str,
        in_reply_to: Option<&str>,
        ref_header: Option<&str>,
    ) -> Self {
        self.mode = Some(mode.to_owned());
        self.in_reply_to = in_reply_to.map(str::to_owned);
        self.ref_header = ref_header.map(str::to_owned);
        self
    }
}

/// Successful save of a message, draft, or template.
#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ZohoMailMutation {
    #[serde(default, deserialize_with = "deserialize_optional_id")]
    pub message_id: Option<String>,
    #[serde(default)]
    pub mail_id: Option<String>,
}

/// Attachment metadata returned by the `attachmentinfo` endpoint.
#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ZohoAttachmentInfo {
    #[serde(default, deserialize_with = "deserialize_optional_id")]
    pub message_id: Option<String>,
    #[serde(default)]
    pub attachments: Vec<ZohoAttachment>,
    #[serde(default)]
    pub inline: Vec<ZohoInlineAttachment>,
}

/// A non-inline attachment available for download.
#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ZohoAttachment {
    #[serde(deserialize_with = "deserialize_required_id")]
    pub attachment_id: String,
    #[serde(default)]
    pub attachment_name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_stringish")]
    pub attachment_size: Option<String>,
}

/// Inline attachment metadata returned only when explicitly requested.
#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ZohoInlineAttachment {
    #[serde(deserialize_with = "deserialize_required_id")]
    pub attachment_id: String,
    #[serde(default)]
    pub attachment_name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_stringish")]
    pub attachment_size: Option<String>,
    #[serde(default)]
    pub cid: Option<String>,
}

/// Sanitized errors from local validation, transport, HTTP status, and Zoho's
/// status/data envelope. Error values never include response bodies, tokens,
/// message addresses, or provider descriptions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ZohoMailClientError {
    InvalidAccessToken,
    ClientInitialization,
    UnsafeTestOrigin,
    InvalidId { field: &'static str },
    InvalidInput { field: &'static str },
    InvalidRequestUrl,
    ReauthRequired,
    Forbidden,
    RateLimited { retry_after_seconds: Option<u32> },
    HttpStatus { status: u16 },
    Business { code: Option<u64> },
    Unavailable { status: Option<u16> },
    Transport,
    InvalidResponse,
    ResponseTooLarge,
}

impl fmt::Display for ZohoMailClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidAccessToken => f.write_str("Zoho Mail access token is invalid"),
            Self::ClientInitialization => {
                f.write_str("Zoho Mail HTTP client could not be initialized")
            }
            Self::UnsafeTestOrigin => f.write_str("Zoho Mail test origin must be loopback HTTP"),
            Self::InvalidId { field } => {
                write!(f, "Zoho Mail {field} must be a numeric provider ID")
            }
            Self::InvalidInput { field } => write!(f, "Zoho Mail {field} is invalid"),
            Self::InvalidRequestUrl => f.write_str("Zoho Mail request URL could not be built"),
            Self::ReauthRequired => f.write_str("Zoho Mail authorization must be renewed"),
            Self::Forbidden => f.write_str("Zoho Mail denied access to this operation"),
            Self::RateLimited { .. } => f.write_str("Zoho Mail rate limit was reached"),
            Self::HttpStatus { status } => write!(f, "Zoho Mail returned HTTP status {status}"),
            Self::Business { code: Some(code) } => {
                write!(f, "Zoho Mail rejected the request with code {code}")
            }
            Self::Business { code: None } => f.write_str("Zoho Mail rejected the request"),
            Self::Unavailable { .. } => f.write_str("Zoho Mail is temporarily unavailable"),
            Self::Transport => f.write_str("Zoho Mail request failed at the transport layer"),
            Self::InvalidResponse => f.write_str("Zoho Mail returned an invalid response"),
            Self::ResponseTooLarge => {
                f.write_str("Zoho Mail response exceeded the configured size limit")
            }
        }
    }
}

impl std::error::Error for ZohoMailClientError {}

fn validate_id(field: &'static str, id: &str) -> Result<(), ZohoMailClientError> {
    if id.is_empty() || id.len() > 32 || !id.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ZohoMailClientError::InvalidId { field });
    }
    Ok(())
}

fn validate_page(start: u32, limit: u16) -> Result<(), ZohoMailClientError> {
    if start == 0 {
        return Err(ZohoMailClientError::InvalidInput { field: "start" });
    }
    if !(1..=200).contains(&limit) {
        return Err(ZohoMailClientError::InvalidInput { field: "limit" });
    }
    Ok(())
}

fn validate_nonempty(field: &'static str, value: &str) -> Result<(), ZohoMailClientError> {
    if value.trim().is_empty() || value.len() > 16 * 1024 {
        return Err(ZohoMailClientError::InvalidInput { field });
    }
    Ok(())
}

fn validate_compose(message: &ZohoMailCompose) -> Result<(), ZohoMailClientError> {
    validate_nonempty("from_address", &message.from_address)?;
    validate_nonempty("to_address", &message.to_address)?;
    if message.subject.len() > 998 || message.content.len() > 512 * 1024 {
        return Err(ZohoMailClientError::InvalidInput { field: "message" });
    }
    if message
        .cc_address
        .as_deref()
        .is_some_and(|value| value.len() > 16 * 1024)
        || message
            .bcc_address
            .as_deref()
            .is_some_and(|value| value.len() > 16 * 1024)
    {
        return Err(ZohoMailClientError::InvalidInput {
            field: "recipients",
        });
    }
    Ok(())
}

fn validate_header_value(field: &'static str, value: &str) -> Result<(), ZohoMailClientError> {
    if value.trim().is_empty() || value.len() > 4096 || value.chars().any(char::is_control) {
        return Err(ZohoMailClientError::InvalidInput { field });
    }
    Ok(())
}

fn validate_message_path(
    account_id: &str,
    folder_id: &str,
    message_id: &str,
) -> Result<(), ZohoMailClientError> {
    validate_id("account_id", account_id)?;
    validate_id("folder_id", folder_id)?;
    validate_id("message_id", message_id)
}

fn classify_http_error(response: &Response) -> ZohoMailClientError {
    match response.status() {
        StatusCode::UNAUTHORIZED => ZohoMailClientError::ReauthRequired,
        StatusCode::FORBIDDEN => ZohoMailClientError::Forbidden,
        StatusCode::TOO_MANY_REQUESTS => ZohoMailClientError::RateLimited {
            retry_after_seconds: response
                .headers()
                .get(header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u32>().ok()),
        },
        status if status.is_server_error() => ZohoMailClientError::Unavailable {
            status: Some(status.as_u16()),
        },
        status => ZohoMailClientError::HttpStatus {
            status: status.as_u16(),
        },
    }
}

async fn read_limited(
    mut response: Response,
    limit: usize,
) -> Result<Vec<u8>, ZohoMailClientError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(ZohoMailClientError::ResponseTooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| ZohoMailClientError::Transport)?
    {
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(ZohoMailClientError::ResponseTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn check_business_status(response: &Value) -> Result<(), ZohoMailClientError> {
    let status = response
        .get("status")
        .and_then(Value::as_object)
        .ok_or(ZohoMailClientError::InvalidResponse)?;
    let raw_code = status
        .get("code")
        .ok_or(ZohoMailClientError::InvalidResponse)?;
    let code = raw_code
        .as_u64()
        .or_else(|| raw_code.as_str().and_then(|value| value.parse().ok()));
    match code {
        Some(200 | 201) => Ok(()),
        Some(401) => Err(ZohoMailClientError::ReauthRequired),
        Some(403) => Err(ZohoMailClientError::Forbidden),
        Some(429) => Err(ZohoMailClientError::RateLimited {
            retry_after_seconds: None,
        }),
        Some(code) if code >= 500 => Err(ZohoMailClientError::Unavailable {
            status: Some(code.min(u16::MAX as u64) as u16),
        }),
        _ => Err(ZohoMailClientError::Business { code }),
    }
}

fn is_json_media_type(content_type: &str) -> bool {
    let media_type = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    media_type == "application/json" || media_type == "text/json" || media_type.ends_with("+json")
}

fn is_zoho_status_envelope(value: &Value) -> bool {
    let has_status_code = value
        .get("status")
        .and_then(Value::as_object)
        .and_then(|status| status.get("code"))
        .is_some_and(|code| {
            code.as_u64().is_some()
                || code
                    .as_str()
                    .is_some_and(|text| text.parse::<u64>().is_ok())
        });
    has_status_code && value.get("data").is_some()
}

fn deserialize_required_id<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    value_to_string(value).ok_or_else(|| serde::de::Error::custom("missing provider ID"))
}

fn deserialize_optional_id<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(value.and_then(value_to_string))
}

fn deserialize_optional_stringish<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(value.and_then(value_to_string))
}

fn value_to_string(value: Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

// Keep the documented US host path visible to downstream reviewers without
// making it configurable in production.
const _: &str = API_BASE;
