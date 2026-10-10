//! The inbox worker must bind a live provider to the exact workspace and
//! mailbox from the job payload. The mock provider never makes network calls.

mod common;

use async_trait::async_trait;
use common::{seed_db, test_db};
use infrastructure::db::Db;
use infrastructure::jobs;
use infrastructure::providers::fake::{FakeDeliveryProvider, FakeMailProvider};
use infrastructure::providers::{
    MailCapabilities, MailProvider, ProviderError, ProviderMessage, SendOutcome, SendRequest,
    SentLookup,
};
use serde_json::json;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use uuid::Uuid;
use worker::{JobRegistry, WorkerContext};

#[derive(Clone)]
struct RoutingMail {
    seen: Arc<Mutex<Vec<(Uuid, Uuid)>>>,
    bound_calls: Arc<AtomicUsize>,
}

struct BoundMail {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl MailProvider for RoutingMail {
    async fn for_mailbox(
        &self,
        workspace_id: Uuid,
        mail_account_id: Uuid,
    ) -> Result<Option<Arc<dyn MailProvider>>, ProviderError> {
        self.seen
            .lock()
            .unwrap()
            .push((workspace_id, mail_account_id));
        Ok(Some(Arc::new(BoundMail {
            calls: self.bound_calls.clone(),
        })))
    }

    fn capabilities(&self) -> MailCapabilities {
        MailCapabilities::default()
    }

    async fn list_messages(
        &self,
        _cursor: Option<String>,
        _limit: u32,
    ) -> Result<(Vec<ProviderMessage>, Option<String>), ProviderError> {
        Err(ProviderError::NotConfigured)
    }

    async fn get_message(
        &self,
        _provider_message_id: &str,
    ) -> Result<ProviderMessage, ProviderError> {
        Err(ProviderError::NotConfigured)
    }

    async fn send(&self, _request: SendRequest) -> Result<SendOutcome, ProviderError> {
        Err(ProviderError::NotConfigured)
    }

    async fn lookup_sent(
        &self,
        _correlation_id: &str,
    ) -> Result<Option<SentLookup>, ProviderError> {
        Err(ProviderError::NotConfigured)
    }
}

#[async_trait]
impl MailProvider for BoundMail {
    fn capabilities(&self) -> MailCapabilities {
        MailCapabilities::default()
    }

    async fn list_messages(
        &self,
        _cursor: Option<String>,
        _limit: u32,
    ) -> Result<(Vec<ProviderMessage>, Option<String>), ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok((Vec::new(), None))
    }

    async fn get_message(
        &self,
        _provider_message_id: &str,
    ) -> Result<ProviderMessage, ProviderError> {
        Err(ProviderError::NotConfigured)
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

#[tokio::test]
async fn inbox_sync_uses_the_mailbox_bound_provider_returned_by_the_router() {
    let db = test_db()
        .await
        .expect("DATABASE_URL must be set for worker tests");
    let fixture_db: Db = seed_db().await;
    let world = infrastructure::testing::seed_workspace(&fixture_db)
        .await
        .unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let bound_calls = Arc::new(AtomicUsize::new(0));
    let mail = Arc::new(RoutingMail {
        seen: seen.clone(),
        bound_calls: bound_calls.clone(),
    });
    let calendar = Arc::new(FakeMailProvider::new());
    let context = WorkerContext {
        db: db.clone(),
        config: Arc::new(infrastructure::config::Config::test()),
        mail,
        delivery: Arc::new(FakeDeliveryProvider::new()),
        calendar,
    };
    let mut registry = JobRegistry::new();
    worker::handlers::inbox::register_with_context(&mut registry, &context);
    let runner = worker::Runner::new(db.clone(), registry);

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    jobs::enqueue(
        &mut tx,
        "inbox.sync_mailbox",
        1,
        json!({"mail_account_id": world.mail_account_id, "reason": "test"}),
        chrono::Utc::now(),
        Some("zoho-binding-test"),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(
        runner.run_workspace_once(world.workspace_id).await.unwrap(),
        1
    );
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        &[(world.workspace_id, world.mail_account_id)]
    );
    assert_eq!(bound_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn provider_message_location_is_workspace_and_mailbox_scoped() {
    let db = test_db()
        .await
        .expect("DATABASE_URL must be set for worker tests");
    let fixture_db = seed_db().await;
    let world = infrastructure::testing::seed_workspace(&fixture_db)
        .await
        .unwrap();
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    infrastructure::repo::mailboxes::record_provider_message_locations(
        &mut tx,
        world.mail_account_id,
        &[("123456789".into(), "987654321".into())],
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let folder = infrastructure::repo::mailboxes::provider_folder_id_for_message(
        &mut tx,
        world.mail_account_id,
        "123456789",
    )
    .await
    .unwrap();
    assert_eq!(folder.as_deref(), Some("987654321"));
    let other_mailbox = infrastructure::repo::mailboxes::provider_folder_id_for_message(
        &mut tx,
        Uuid::new_v4(),
        "123456789",
    )
    .await
    .unwrap();
    assert!(other_mailbox.is_none());
    tx.rollback().await.unwrap();

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let invalid = infrastructure::repo::mailboxes::record_provider_message_locations(
        &mut tx,
        world.mail_account_id,
        &[("123/path".into(), "987654321".into())],
    )
    .await;
    assert!(invalid.is_err());
    tx.rollback().await.unwrap();
}
