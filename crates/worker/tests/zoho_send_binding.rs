//! Sending resolves the Zoho account from the outbound mailbox ID before the
//! submitting boundary. The adapter is local and records which instance got
//! the send; no provider HTTP request is made by this test.

mod common;

use common::{seed_db, test_db};
use contracts::sending::{
    CreateMessageDraftRequest, FreezeMessageDraftRequest, SubmitOutboundRequest,
};
use infrastructure::db::{Db, ROLE_API, Tx};
use infrastructure::providers::fake::{FakeDeliveryProvider, FakeMailProvider};
use infrastructure::providers::{
    MailCapabilities, MailProvider, ProviderError, ProviderMessage, SendOutcome, SendRequest,
    SentLookup,
};
use infrastructure::testing::{TestWorld, ctx_for, seed_workspace};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use uuid::Uuid;
use worker::{JobRegistry, Runner, WorkerContext};

#[derive(Clone)]
struct RoutingMail {
    seen: Arc<Mutex<Vec<(Uuid, Uuid)>>>,
    bound_sends: Arc<AtomicUsize>,
    unbound_sends: Arc<AtomicUsize>,
}

struct BoundMail {
    sends: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
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
            sends: self.bound_sends.clone(),
        })))
    }

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
        Err(ProviderError::NotConfigured)
    }

    async fn get_message(
        &self,
        _provider_message_id: &str,
    ) -> Result<ProviderMessage, ProviderError> {
        Err(ProviderError::NotConfigured)
    }

    async fn send(&self, _request: SendRequest) -> Result<SendOutcome, ProviderError> {
        self.unbound_sends.fetch_add(1, Ordering::SeqCst);
        Ok(SendOutcome::Accepted {
            provider_message_id: "unbound-message".into(),
        })
    }

    async fn lookup_sent(
        &self,
        _correlation_id: &str,
    ) -> Result<Option<SentLookup>, ProviderError> {
        Ok(None)
    }
}

#[async_trait::async_trait]
impl MailProvider for BoundMail {
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
        Ok((Vec::new(), None))
    }

    async fn get_message(
        &self,
        _provider_message_id: &str,
    ) -> Result<ProviderMessage, ProviderError> {
        Err(ProviderError::NotConfigured)
    }

    async fn send(&self, _request: SendRequest) -> Result<SendOutcome, ProviderError> {
        self.sends.fetch_add(1, Ordering::SeqCst);
        Ok(SendOutcome::Accepted {
            provider_message_id: "123456789".into(),
        })
    }

    async fn lookup_sent(
        &self,
        _correlation_id: &str,
    ) -> Result<Option<SentLookup>, ProviderError> {
        Ok(None)
    }
}

async fn sender_and_channel(tx: &mut Tx<'_>, world: &TestWorld, recipient: Uuid) -> Uuid {
    let sender: Uuid = sqlx::query_scalar(
        "INSERT INTO sender_identities
            (workspace_id, mail_account_id, address, display_name, status, verified_at)
         VALUES ($1,$2,$3,'Sender','verified',now()) RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(world.mail_account_id)
    .bind("sender@example.test")
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let channel: Uuid = sqlx::query_scalar(
        "INSERT INTO delivery_channels
            (workspace_id, mail_account_id, sender_identity_id, connection_id,
             provider, purpose, name, policy_status, enabled)
         VALUES ($1,$2,$3,$4,'zoho_mail','correspondence','test','allowed',true)
         RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(world.mail_account_id)
    .bind(sender)
    .bind(world.connection_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO communication_bases
            (workspace_id,email_address_id,purpose,jurisdiction,basis_type,status,
             evidence_ref,collected_at,reviewed_by,reviewed_at)
         VALUES ($1,$2,'correspondence','KR','explicit_consent','valid','test',now(),$3,now())",
    )
    .bind(world.workspace_id)
    .bind(recipient)
    .bind(world.admin.principal_id)
    .execute(tx.conn())
    .await
    .unwrap();
    channel
}

async fn recipient(tx: &mut Tx<'_>) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO email_addresses(workspace_id,address_original,normalized_key)
         VALUES($1,$2,$2) RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(format!(
        "recipient-{}@example.test",
        Uuid::new_v4().simple()
    ))
    .fetch_one(tx.conn())
    .await
    .unwrap()
}

async fn enable_sending(workspace_id: Uuid) {
    let database_url = std::env::var("DATABASE_URL").unwrap();
    let api_db = Db::connect(&database_url, Some(ROLE_API)).await.unwrap();
    let mut tx = api_db.begin(workspace_id).await.unwrap();
    sqlx::query("UPDATE workspaces SET emergency_stop=false WHERE id=$1")
        .bind(workspace_id)
        .execute(tx.conn())
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn dispatch_sends_only_through_the_provider_bound_to_the_outbound_mailbox() {
    let db = test_db()
        .await
        .expect("DATABASE_URL must be set for worker tests");
    let seed = seed_db().await;
    let world = seed_workspace(&seed).await.unwrap();
    enable_sending(world.workspace_id).await;
    let ctx = ctx_for(&world, &world.admin, "zoho-dispatch-binding");

    let (channel, recipient_id) = {
        let mut tx = db.begin(world.workspace_id).await.unwrap();
        let recipient = recipient(&mut tx).await;
        let channel = sender_and_channel(&mut tx, &world, recipient).await;
        tx.commit().await.unwrap();
        (channel, recipient)
    };
    let draft = application::sending::create_draft(
        &db,
        &ctx,
        &CreateMessageDraftRequest {
            mail_account_id: world.mail_account_id,
            to_email_address_id: recipient_id,
            purpose: "correspondence".into(),
            subject: "A real account binding".into(),
            body_text: "Hello".into(),
            body_html: None,
            template_version_id: None,
            variables: None,
            thread_id: None,
            reply_to_message_id: None,
            expected_last_inbound_id: None,
            visibility: None,
        },
    )
    .await
    .unwrap();
    let frozen = application::sending::freeze_draft(
        &db,
        &ctx,
        draft.draft_id,
        1,
        &FreezeMessageDraftRequest {
            delivery_channel_id: channel,
            sender_identity_id: None,
            meeting_type_id: None,
            tracking_requested: false,
        },
    )
    .await
    .unwrap();
    application::sending::submit(
        &db,
        &ctx,
        frozen.outbound_message_id,
        &SubmitOutboundRequest {
            expected_payload_hash: frozen.payload_hash,
            not_before: None,
        },
        "zoho-dispatch-binding-submit",
    )
    .await
    .unwrap();

    let seen = Arc::new(Mutex::new(Vec::new()));
    let bound_sends = Arc::new(AtomicUsize::new(0));
    let unbound_sends = Arc::new(AtomicUsize::new(0));
    let mail = Arc::new(RoutingMail {
        seen: seen.clone(),
        bound_sends: bound_sends.clone(),
        unbound_sends: unbound_sends.clone(),
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
    worker::handlers::sending::register_with_context(&mut registry, &context);
    let runner = Runner::new(db.clone(), registry);

    assert_eq!(
        runner.run_workspace_once(world.workspace_id).await.unwrap(),
        1
    );
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        &[(world.workspace_id, world.mail_account_id)]
    );
    assert_eq!(bound_sends.load(Ordering::SeqCst), 1);
    assert_eq!(unbound_sends.load(Ordering::SeqCst), 0);

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let outbound = infrastructure::repo::sending::get_outbound(&mut tx, frozen.outbound_message_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(outbound.status, "accepted");
    assert_eq!(outbound.provider_message_id.as_deref(), Some("123456789"));
    tx.rollback().await.unwrap();
}
