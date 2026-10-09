//! Shared application state for the Axum app.

use std::sync::Arc;

use infrastructure::config::Config;
use infrastructure::db::Db;
use infrastructure::providers::{CalendarProvider, DeliveryProvider, MailProvider};
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
    /// Standard dev/test wiring: Fake providers, optional limiter.
    pub fn with_fake_providers(
        db: Db,
        config: Arc<Config>,
        rate_limiter: Option<RateLimiter>,
    ) -> Self {
        // Fake providers are scripted by Config.fake_provider_script (R27).
        let fake = Arc::new(match &config.fake_provider_script {
            Some(path) => infrastructure::providers::fake::FakeMailProvider::from_script(path),
            None => infrastructure::providers::fake::FakeMailProvider::new(),
        });
        let delivery = Arc::new(match &config.fake_provider_script {
            Some(path) => infrastructure::providers::fake::FakeDeliveryProvider::from_script(path),
            None => infrastructure::providers::fake::FakeDeliveryProvider::new(),
        });
        Self {
            db,
            config,
            rate_limiter,
            mail: fake.clone(),
            calendar: fake.clone(),
            delivery,
            fake_mail: Some(fake),
        }
    }
}
