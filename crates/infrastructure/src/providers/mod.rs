//! External provider abstraction boundary (spec §4.9, §6.3-B/C/D, source
//! §10.2). Handlers and use cases depend on these traits only — provider SDK
//! details never leak past this module.
//!
//! `SendOutcome::Unknown` is a first-class outcome: callers must mark the
//! business row `unknown` and reconcile via `lookup_sent`; never auto-retry.

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

pub mod fake;
pub mod office_calendar;
pub mod zoho;
pub use office_calendar::{CalendarProviderRouter, OfficeCalendarProvider};

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

/// Errors for provider paths.
#[derive(Debug, Clone)]
pub enum ProviderError {
    /// Outage/timeout/5xx — retryable by the caller's policy.
    Unavailable(String),
    /// Adapter has no credential/config — a permanent condition, never a
    /// retryable `Unknown` (R27).
    NotConfigured,
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
    /// Resolve a workspace/mailbox-bound live client. Fakes are already bound
    /// by the caller and return None. Never infer tenant identity from From.
    async fn for_mailbox(
        &self,
        _workspace_id: uuid::Uuid,
        _mail_account_id: uuid::Uuid,
    ) -> Result<Option<std::sync::Arc<dyn MailProvider>>, ProviderError> {
        Ok(None)
    }
    /// A live polling adapter can limit pages per job while retaining cursor progress.
    fn sync_page_budget(&self) -> usize {
        20
    }

    fn capabilities(&self) -> MailCapabilities;
    /// Page provider messages; `cursor` is provider-owned opaque state.
    async fn list_messages(
        &self,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<(Vec<ProviderMessage>, Option<String>), ProviderError>;
    async fn get_message(
        &self,
        provider_message_id: &str,
    ) -> Result<ProviderMessage, ProviderError>;
    /// Submit an outbound message. `correlation_id` is our idempotency
    /// anchor the fake provider echoes into custom headers. `Err` =
    /// transport/config failure (e.g. `NotConfigured`) — distinct from
    /// `SendOutcome::Unknown` which means "maybe delivered, reconcile".
    async fn send(&self, req: SendRequest) -> Result<SendOutcome, ProviderError>;
    /// Reconcile an `Unknown` outcome by correlation id / recipient+subject.
    async fn lookup_sent(&self, correlation_id: &str) -> Result<Option<SentLookup>, ProviderError>;
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
// Scoped operations intentionally carry trusted workspace/booking/change IDs
// separately from the proposed event fields.
#[allow(clippy::too_many_arguments)]
#[async_trait]
pub trait CalendarProvider: Send + Sync {
    async fn free_busy(
        &self,
        host: &str,
        range_start: DateTime<Utc>,
        range_end: DateTime<Utc>,
    ) -> CalOutcome<Vec<BusyBlock>>;
    /// Workspace-aware availability query. External adapters keep their
    /// existing contract; Office's local adapter uses this scope to reject a
    /// calendar id copied from another workspace and to ignore the booking's
    /// own active hold while confirming or changing it.
    async fn free_busy_scoped(
        &self,
        workspace_id: uuid::Uuid,
        host: &str,
        range_start: DateTime<Utc>,
        range_end: DateTime<Utc>,
        _exclude_booking_id: Option<uuid::Uuid>,
        _exclude_change_id: Option<uuid::Uuid>,
    ) -> CalOutcome<Vec<BusyBlock>> {
        let _ = workspace_id;
        self.free_busy(host, range_start, range_end).await
    }
    async fn create_event(
        &self,
        host: &str,
        title: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        attendee_email: &str,
    ) -> CalOutcome<CalEvent>;
    /// Create a calendar event for a specific Office booking. The default
    /// keeps existing providers source-compatible; local storage overrides
    /// it to make creation idempotent by booking UUID.
    async fn create_booking_event(
        &self,
        host: &str,
        _booking_id: uuid::Uuid,
        title: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        attendee_email: &str,
    ) -> CalOutcome<CalEvent> {
        self.create_event(host, title, start, end, attendee_email)
            .await
    }
    /// Workspace-aware variant used by booking flows. Existing provider
    /// adapters receive the same booking-id method through the default.
    async fn create_booking_event_scoped(
        &self,
        workspace_id: uuid::Uuid,
        host: &str,
        booking_id: uuid::Uuid,
        title: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        attendee_email: &str,
    ) -> CalOutcome<CalEvent> {
        let _ = workspace_id;
        self.create_booking_event(host, booking_id, title, start, end, attendee_email)
            .await
    }
    /// Reschedule/update an existing provider event (booking reschedule).
    async fn update_event(
        &self,
        host: &str,
        provider_event_id: &str,
        title: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> CalOutcome<CalEvent>;
    /// Workspace and booking binding for a reschedule. The fallback preserves
    /// older provider implementations while local storage validates all IDs.
    #[allow(clippy::too_many_arguments)]
    async fn update_event_scoped(
        &self,
        workspace_id: uuid::Uuid,
        host: &str,
        _booking_id: uuid::Uuid,
        _change_id: uuid::Uuid,
        provider_event_id: &str,
        title: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        _occupied_start: DateTime<Utc>,
        _occupied_end: DateTime<Utc>,
    ) -> CalOutcome<CalEvent> {
        let _ = workspace_id;
        self.update_event(host, provider_event_id, title, start, end)
            .await
    }
    async fn cancel_event(&self, host: &str, provider_event_id: &str) -> CalOutcome<()>;
    /// Workspace and booking binding for cancellation; old adapters continue
    /// to use their existing event-id implementation.
    async fn cancel_event_scoped(
        &self,
        workspace_id: uuid::Uuid,
        host: &str,
        _booking_id: uuid::Uuid,
        _change_id: uuid::Uuid,
        provider_event_id: &str,
    ) -> CalOutcome<()> {
        let _ = workspace_id;
        self.cancel_event(host, provider_event_id).await
    }
}

/// Fail-closed fallback used when no legacy calendar adapter is configured.
/// Local Office calendar ids are handled by `CalendarProviderRouter` first.
pub struct DisabledCalendarProvider;

#[async_trait]
impl CalendarProvider for DisabledCalendarProvider {
    async fn free_busy(
        &self,
        _host: &str,
        _range_start: DateTime<Utc>,
        _range_end: DateTime<Utc>,
    ) -> CalOutcome<Vec<BusyBlock>> {
        CalOutcome::Failed {
            code: "NOT_CONFIGURED".into(),
            message: "No external calendar adapter is configured".into(),
        }
    }

    async fn create_event(
        &self,
        _host: &str,
        _title: &str,
        _start: DateTime<Utc>,
        _end: DateTime<Utc>,
        _attendee_email: &str,
    ) -> CalOutcome<CalEvent> {
        CalOutcome::Failed {
            code: "NOT_CONFIGURED".into(),
            message: "No external calendar adapter is configured".into(),
        }
    }

    async fn update_event(
        &self,
        _host: &str,
        _provider_event_id: &str,
        _title: &str,
        _start: DateTime<Utc>,
        _end: DateTime<Utc>,
    ) -> CalOutcome<CalEvent> {
        CalOutcome::Failed {
            code: "NOT_CONFIGURED".into(),
            message: "No external calendar adapter is configured".into(),
        }
    }

    async fn cancel_event(&self, _host: &str, _provider_event_id: &str) -> CalOutcome<()> {
        CalOutcome::Failed {
            code: "NOT_CONFIGURED".into(),
            message: "No external calendar adapter is configured".into(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct DeliveryCapabilities {
    pub supports_send: bool,
    /// Accepts Office's one-click headers. Relay DKIM coverage and native
    /// recipient-client UI support must be verified separately.
    pub one_click_unsubscribe: bool,
    pub custom_headers: bool,
    /// Provider-level idempotency keys accepted.
    pub idempotency: bool,
}

/// Office-owned marketing/announcement delivery through a reviewed channel.
#[async_trait]
pub trait DeliveryProvider: Send + Sync {
    /// Bind a transport using tenant and channel identity, never a From header.
    async fn for_channel(
        &self,
        _workspace_id: uuid::Uuid,
        _channel_id: uuid::Uuid,
    ) -> Result<Option<std::sync::Arc<dyn DeliveryProvider>>, ProviderError> {
        Ok(None)
    }
    fn capabilities(&self) -> DeliveryCapabilities;
    async fn send(&self, req: SendRequest) -> Result<SendOutcome, ProviderError>;
    async fn lookup_sent(&self, correlation_id: &str) -> Result<Option<SentLookup>, ProviderError>;
}

pub mod smtp;
pub mod zoho_mail_client;
