//! Zoho adapter skeletons (spec §4.9): compile-checked trait impls that
//! return `ProviderError::Unavailable` unless a configured credential is
//! supplied. Real API calls are a later module task; the `reqwest` client is
//! constructed here so the dependency shape is proven.

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use super::*;

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
            supports_send: true,
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
