//! Shared application state for the Axum app.

use std::sync::Arc;

use infrastructure::config::Config;
use infrastructure::db::Db;
#[cfg(test)]
use infrastructure::providers::CalOutcome;
use infrastructure::providers::{
    CalendarProvider, CalendarProviderRouter, DeliveryCapabilities, DeliveryProvider,
    DisabledCalendarProvider, MailProvider, ProviderError, SendOutcome, SendRequest, SentLookup,
};
use infrastructure::ratelimit::RateLimiter;

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub config: Arc<Config>,
    /// None when REDIS_URL is unset — call sites treat absence as
    /// "limiter not configured" and skip (dev/test only decision; in prod a
    /// missing Redis URL is a startup error).
    pub rate_limiter: Option<RateLimiter>,
    pub mail: Arc<dyn MailProvider>,
    pub calendar: Arc<dyn CalendarProvider>,
    pub delivery: Arc<dyn DeliveryProvider>,
    /// When the mail provider is the in-memory fake, the dev endpoint uses
    /// this handle to inject inbound messages (spec §4.9).
    pub fake_mail: Option<Arc<infrastructure::providers::fake::FakeMailProvider>>,
}

impl AppState {
    /// Build runtime providers. Fake adapters are used only for explicit test
    /// fixtures or an unconfigured development server; other environments
    /// receive fail-closed live providers.
    pub fn with_fake_providers(
        db: Db,
        config: Arc<Config>,
        rate_limiter: Option<RateLimiter>,
    ) -> Self {
        Self::from_config(db, config, rate_limiter)
    }

    /// Build the application provider set for the current runtime config.
    pub fn from_config(db: Db, config: Arc<Config>, rate_limiter: Option<RateLimiter>) -> Self {
        let use_fake = config.zoho.fake_for_tests
            || (config.app_env == infrastructure::config::AppEnv::Dev
                && !application::mailboxes::oauth::mail_oauth_settings_present(&config));
        let fake = use_fake.then(|| {
            Arc::new(match &config.fake_provider_script {
                Some(path) => infrastructure::providers::fake::FakeMailProvider::from_script(path),
                None => infrastructure::providers::fake::FakeMailProvider::new(),
            })
        });
        let delivery_fake = use_fake.then(|| {
            Arc::new(match &config.fake_provider_script {
                Some(path) => {
                    infrastructure::providers::fake::FakeDeliveryProvider::from_script(path)
                }
                None => infrastructure::providers::fake::FakeDeliveryProvider::new(),
            })
        });
        let mail: Arc<dyn MailProvider> = fake
            .as_ref()
            .map(|provider| provider.clone() as Arc<dyn MailProvider>)
            .unwrap_or_else(|| {
                Arc::new(application::mailboxes::MailProviderRouter::new(
                    db.clone(),
                    config.clone(),
                ))
            });
        let calendar_fallback: Arc<dyn CalendarProvider> = fake
            .as_ref()
            .map(|provider| provider.clone() as Arc<dyn CalendarProvider>)
            .unwrap_or_else(|| Arc::new(DisabledCalendarProvider));
        let calendar: Arc<dyn CalendarProvider> =
            Arc::new(CalendarProviderRouter::new(db.clone(), calendar_fallback));
        let delivery_fallback: Arc<dyn DeliveryProvider> = delivery_fake
            .as_ref()
            .map(|provider| provider.clone() as Arc<dyn DeliveryProvider>)
            .unwrap_or_else(|| Arc::new(DisabledDeliveryProvider));
        let delivery: Arc<dyn DeliveryProvider> =
            if config.zoho.fake_for_tests || config.fake_provider_script.is_some() {
                delivery_fallback
            } else {
                Arc::new(application::smtp::DeliveryRouter::new(
                    db.clone(),
                    config.clone(),
                    delivery_fallback,
                ))
            };
        Self {
            db,
            config,
            rate_limiter,
            mail,
            calendar,
            delivery,
            fake_mail: fake,
        }
    }
}

struct DisabledDeliveryProvider;

#[async_trait::async_trait]
impl DeliveryProvider for DisabledDeliveryProvider {
    fn capabilities(&self) -> DeliveryCapabilities {
        DeliveryCapabilities::default()
    }

    async fn send(&self, _request: SendRequest) -> Result<SendOutcome, ProviderError> {
        Err(ProviderError::NotConfigured)
    }

    async fn lookup_sent(
        &self,
        _correlation_id: &str,
    ) -> Result<Option<SentLookup>, ProviderError> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lazy_db() -> Db {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://test:test@127.0.0.1/outreach_test")
            .unwrap();
        Db::new(pool, None, None)
    }

    #[tokio::test]
    async fn explicit_fake_fixture_uses_fake_but_test_env_alone_fails_closed() {
        let fake = AppState::with_fake_providers(lazy_db(), Arc::new(Config::test()), None);
        assert!(fake.fake_mail.is_some());
        assert!(fake.mail.capabilities().supports_send);

        let mut config = Config::test();
        config.zoho.fake_for_tests = false;
        let live_test = AppState::with_fake_providers(lazy_db(), Arc::new(config), None);
        assert!(live_test.fake_mail.is_none());
        assert!(!live_test.mail.capabilities().supports_send);
        assert!(!live_test.delivery.capabilities().supports_send);
        assert!(matches!(
            live_test.calendar.free_busy(
                "host@example.test",
                chrono::Utc::now(),
                chrono::Utc::now() + chrono::Duration::hours(1)
            ).await,
            CalOutcome::Failed { ref code, .. } if code == "NOT_CONFIGURED"
        ));

        let mut partial_dev = Config::test();
        partial_dev.app_env = infrastructure::config::AppEnv::Dev;
        partial_dev.zoho.fake_for_tests = false;
        partial_dev.zoho.mail_client_id = Some("1000.partial-mail-client".into());
        let partial = AppState::with_fake_providers(lazy_db(), Arc::new(partial_dev), None);
        assert!(partial.fake_mail.is_none());
        assert!(!partial.mail.capabilities().supports_send);
    }
}
