//! External provider abstraction boundary (spec §4.9, §6.3-B/C/D, source
//! §10.2). Handlers and use cases depend on these traits only — provider SDK
//! details never leak past this module.
//!
//! `SendOutcome::Unknown` is a first-class outcome: callers must mark the
//! business row `unknown` and reconcile via `lookup_sent`; never auto-retry.

pub mod fake;
pub mod zoho;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Three-valued submission result (source B.7). `Unknown` = the request may
/// or may not have reached the provider — reconcile, don't retry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SendOutcome {
    Accepted { provider_message_id: String },
    Rejected { permanent: bool, code: String },
    Unknown { reason: String },
}

/// Errors for provider read paths.
#[derive(Debug, Clone)]
pub enum ProviderError {
    /// Outage/timeout/5xx — retryable by the caller's policy.
    Unavailable(String),
    /// OAuth token invalid/revoked → `INTEGRATION_REAUTH_REQUIRED`.
    ReauthRequired,
    /// Provider-side throttle.
    RateLimited { retry_after_seconds: u32 },
}

impl ProviderError {
    pub fn unavailable(msg: impl Into<String>) -> Self {
        Self::Unavailable(msg.into())
    }
}

/// Normalized inbound/observed message (provider-neutral).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderMessage {
    pub provider_message_id: String,
    pub provider_thread_id: Option<String>,
    pub internet_message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub from: String,
    pub to: Vec<String>,
    pub subject: Option<String>,
    /// Plain-text body (sanitized by the caller before storage).
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    pub message_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default)]
pub struct MailCapabilities {
    pub supports_send: bool,
    pub supports_delta_sync: bool,
}

/// Result of reconciling an `unknown` send.
#[derive(Debug, Clone)]
pub struct SentLookup {
    pub provider_message_id: String,
    pub sent_at: DateTime<Utc>,
}

/// Mailbox provider: sync + send (source §6.3-B, B.7).
#[async_trait]
pub trait MailProvider: Send + Sync {
    fn capabilities(&self) -> MailCapabilities;
    /// Page provider messages; `cursor` is provider-owned opaque state.
    async fn list_messages(
        &self,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<(Vec<ProviderMessage>, Option<String>), ProviderError>;
    async fn get_message(&self, provider_message_id: &str)
        -> Result<ProviderMessage, ProviderError>;
    /// Submit an outbound message. `correlation_id` is our idempotency
    /// anchor the fake provider echoes into custom headers.
    async fn send(&self, req: SendRequest) -> SendOutcome;
    /// Reconcile an `Unknown` outcome by correlation id / recipient+subject.
    async fn lookup_sent(&self, correlation_id: &str)
        -> Result<Option<SentLookup>, ProviderError>;
}

#[derive(Debug, Clone)]
pub struct SendRequest {
    pub correlation_id: String,
    pub from: String,
    pub to: String,
    pub subject: String,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    /// Message-ID we are replying to, when threaded.
    pub in_reply_to: Option<String>,
    pub headers: Vec<(String, String)>,
}

/// Calendar outcome mirrors provider honesty: Ok / Failed / Unknown.
#[derive(Debug, Clone)]
pub enum CalOutcome<T> {
    Ok(T),
    Failed { code: String, message: String },
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusyBlock {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct CalEvent {
    pub provider_event_id: String,
    pub revision: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

/// Host calendar provider (source §6.3-C).
#[async_trait]
pub trait CalendarProvider: Send + Sync {
    async fn free_busy(
        &self,
        host: &str,
        range_start: DateTime<Utc>,
        range_end: DateTime<Utc>,
    ) -> CalOutcome<Vec<BusyBlock>>;
    async fn create_event(
        &self,
        host: &str,
        title: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        attendee_email: &str,
    ) -> CalOutcome<CalEvent>;
    async fn cancel_event(&self, host: &str, provider_event_id: &str) -> CalOutcome<()>;
}

#[derive(Debug, Clone, Default)]
pub struct DeliveryCapabilities {
    pub one_click_unsubscribe: bool,
    pub custom_headers: bool,
    /// Provider-level idempotency keys accepted.
    pub idempotency: bool,
}

/// Transactional delivery path (source §6.3-D): marketing/announcement mail
/// that must not flow through user mailboxes.
#[async_trait]
pub trait DeliveryProvider: Send + Sync {
    fn capabilities(&self) -> DeliveryCapabilities;
    async fn send(&self, req: SendRequest) -> SendOutcome;
    async fn lookup_sent(
        &self,
        correlation_id: &str,
    ) -> Result<Option<SentLookup>, ProviderError>;
}
