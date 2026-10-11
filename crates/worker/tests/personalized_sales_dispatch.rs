//! Personalized sales queue → durable dispatch integration using fake
//! providers. No live transport is configured or called.

use std::sync::Arc;

use application::{sales_campaigns, sending};
use chrono::{DateTime, Duration, Utc};
use contracts::{
    sales_campaigns::{
        AddSalesCampaignTargetRequest, CreateSalesCampaignDraftRequest, CreateSalesCampaignRequest,
        QueueSalesCampaignTargetRequest,
    },
    sending::FreezeMessageDraftRequest,
};
use infrastructure::{
    config::Config,
    db::{Db, ROLE_API, ROLE_WORKER},
    providers::fake::FakeMailProvider,
    providers::{
        DeliveryCapabilities, DeliveryProvider, MailProvider, ProviderError, SendOutcome,
        SendRequest, SentLookup,
    },
    testing::{TestWorld, ctx_for, seed_workspace},
};
use uuid::Uuid;
use worker::{JobRegistry, Runner, WorkerContext};

async fn test_databases() -> Option<(Db, Db, TestWorld)> {
    let url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping sales dispatch integration");
            return None;
        }
    };
    let database = url
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .split('?')
        .next()
        .unwrap_or_default();
    assert_eq!(
        database, "outreach_sales_kanban_api",
        "sales dispatch test must use the dedicated sales API test database"
    );
    let owner = Db::connect(&url, None).await.unwrap();
    owner.migrate().await.unwrap();
    let api_db = Db::connect(&url, Some(ROLE_API)).await.unwrap();
    let world = seed_workspace(&api_db).await.unwrap();
    let mut tx = api_db.begin(world.workspace_id).await.unwrap();
    sqlx::query("UPDATE workspaces SET emergency_stop = false WHERE id = $1")
        .bind(world.workspace_id)
        .execute(tx.conn())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let worker_db = Db::connect(&url, Some(ROLE_WORKER)).await.unwrap();
    Some((api_db, worker_db, world))
}

#[derive(Default)]
struct RecordingDelivery {
    sends: std::sync::Mutex<Vec<SendRequest>>,
    outcomes: std::sync::Mutex<std::collections::VecDeque<SendOutcome>>,
}

impl RecordingDelivery {
    fn with_outcome(outcome: SendOutcome) -> Self {
        Self {
            sends: std::sync::Mutex::new(Vec::new()),
            outcomes: std::sync::Mutex::new(std::collections::VecDeque::from([outcome])),
        }
    }

    fn send_count(&self) -> usize {
        self.sends.lock().unwrap().len()
    }
}

#[async_trait::async_trait]
impl DeliveryProvider for RecordingDelivery {
    fn capabilities(&self) -> DeliveryCapabilities {
        DeliveryCapabilities {
            supports_send: true,
            ..Default::default()
        }
    }

    async fn send(&self, request: SendRequest) -> Result<SendOutcome, ProviderError> {
        self.sends.lock().unwrap().push(request);
        Ok(self
            .outcomes
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| SendOutcome::Accepted {
                provider_message_id: format!("sales-fake-{}", Uuid::new_v4()),
            }))
    }

    async fn lookup_sent(
        &self,
        _correlation_id: &str,
    ) -> Result<Option<SentLookup>, ProviderError> {
        Ok(None)
    }
}

async fn create_sender(db: &Db, world: &TestWorld, suffix: &str) -> (Uuid, Uuid, Uuid) {
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let profile_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO integration_connections
            (id,workspace_id,provider,region,secret_ref,status,connected_by)
         VALUES ($1,$2,'delivery_provider','smtp',$3,'active',$4)",
    )
    .bind(profile_id)
    .bind(world.workspace_id)
    .bind(format!("fake://sales-dispatch/{suffix}"))
    .bind(world.admin.principal_id)
    .execute(tx.conn())
    .await
    .unwrap();
    let sender_id: Uuid = sqlx::query_scalar(
        "INSERT INTO sender_identities
            (workspace_id,mail_account_id,address,display_name,status,verified_at)
         VALUES ($1,$2,$3,$4,'verified',now()) RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(world.mail_account_id)
    .bind(format!("sales-{suffix}@example.test"))
    .bind(format!("Sales {suffix}"))
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let channel_id: Uuid = sqlx::query_scalar(
        "INSERT INTO delivery_channels
            (workspace_id,mail_account_id,sender_identity_id,connection_id,
             provider,purpose,name,policy_status,enabled)
         VALUES ($1,$2,$3,$4,'delivery_provider','marketing',$5,'allowed',true)
         RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(world.mail_account_id)
    .bind(sender_id)
    .bind(profile_id)
    .bind(format!("Sales channel {suffix}"))
    .fetch_one(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO smtp_connections
            (id,workspace_id,mail_account_id,sender_identity_id,channel_id,name,host,port,
             security,username,from_address,from_name,reply_to,status,has_password)
         VALUES ($1,$2,$3,$4,$5,$6,'smtp.example.test',587,'starttls','smtp-user',
                 $7,$8,$9,'active',true)",
    )
    .bind(profile_id)
    .bind(world.workspace_id)
    .bind(world.mail_account_id)
    .bind(sender_id)
    .bind(channel_id)
    .bind(format!("Sales profile {suffix}"))
    .bind(format!("sales-{suffix}@example.test"))
    .bind(format!("Sales {suffix}"))
    .bind(format!("reply-{suffix}@example.test"))
    .execute(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO sales_campaign_sender_defaults (workspace_id,smtp_profile_id,updated_by)
         VALUES ($1,$2,$3)
         ON CONFLICT (workspace_id) DO UPDATE
            SET smtp_profile_id = EXCLUDED.smtp_profile_id,
                updated_by = EXCLUDED.updated_by,
                version = sales_campaign_sender_defaults.version + 1,
                updated_at = now()",
    )
    .bind(world.workspace_id)
    .bind(profile_id)
    .bind(world.admin.principal_id)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    (profile_id, channel_id, sender_id)
}

async fn create_contact(db: &Db, world: &TestWorld, suffix: &str) -> (Uuid, Uuid, Uuid) {
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let company_id: Uuid =
        sqlx::query_scalar("INSERT INTO companies (workspace_id,name) VALUES ($1,$2) RETURNING id")
            .bind(world.workspace_id)
            .bind(format!("Company {suffix}"))
            .fetch_one(tx.conn())
            .await
            .unwrap();
    let contact_id: Uuid = sqlx::query_scalar(
        "INSERT INTO contacts (workspace_id,company_id,name,status)
         VALUES ($1,$2,$3,'active') RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(company_id)
    .bind(format!("Contact {suffix}"))
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let address = format!("recipient-{suffix}@example.test");
    let email_id: Uuid = sqlx::query_scalar(
        "INSERT INTO email_addresses (workspace_id,address_original,normalized_key)
         VALUES ($1,$2,$3) RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(&address)
    .bind(address.to_ascii_lowercase())
    .fetch_one(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO contact_emails (workspace_id,contact_id,email_address_id,is_primary)
         VALUES ($1,$2,$3,true)",
    )
    .bind(world.workspace_id)
    .bind(contact_id)
    .bind(email_id)
    .execute(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO communication_bases
            (workspace_id,email_address_id,purpose,jurisdiction,basis_type,status,
             evidence_ref,collected_at,reviewed_by,reviewed_at)
         VALUES ($1,$2,'marketing','US','explicit_consent','valid',$3,now(),$4,now())",
    )
    .bind(world.workspace_id)
    .bind(email_id)
    .bind(format!("test-consent:{suffix}"))
    .bind(world.admin.principal_id)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    (company_id, contact_id, email_id)
}

async fn queued_sales_message(
    api_db: &Db,
    world: &TestWorld,
    outcome_suffix: &str,
) -> (Uuid, Uuid, Uuid, DateTime<Utc>, String) {
    let admin = ctx_for(world, &world.admin, "sales-dispatch-admin");
    let (_profile_id, channel_id, sender_id) = create_sender(api_db, world, outcome_suffix).await;
    let (company_id, contact_id, email_id) = create_contact(api_db, world, outcome_suffix).await;

    let mut tx = api_db.begin(world.workspace_id).await.unwrap();
    let campaign = sales_campaigns::create_sales_campaign(
        &mut tx,
        &admin,
        &CreateSalesCampaignRequest {
            name: format!("Sales {outcome_suffix}"),
            objective: Some("Book a demo".into()),
            meeting_type_id: None,
        },
    )
    .await
    .unwrap();
    let campaign_id = campaign.id;
    tx.commit().await.unwrap();

    let mut tx = api_db.begin(world.workspace_id).await.unwrap();
    let target = sales_campaigns::add_sales_campaign_target(
        &mut tx,
        &admin,
        campaign_id,
        &AddSalesCampaignTargetRequest {
            company_id,
            contact_id,
            email_address_id: email_id,
        },
    )
    .await
    .unwrap();
    let target_id = target.id;
    tx.commit().await.unwrap();

    let draft_request = CreateSalesCampaignDraftRequest {
        subject: format!("Hello {outcome_suffix}"),
        body_text: format!("Company-specific note for {outcome_suffix}."),
        body_html: None,
        template_version_id: None,
        variables: None,
        explicit_follow_up: false,
    };
    let tx = api_db.begin(world.workspace_id).await.unwrap();
    let (mut tx, draft) = sales_campaigns::create_sales_campaign_target_draft(
        tx,
        &admin,
        campaign_id,
        target_id,
        &draft_request,
    )
    .await
    .unwrap();
    let draft_id = draft.draft_id;
    let target_version = {
        let row: i64 = sqlx::query_scalar(
            "SELECT version FROM sales_campaign_targets WHERE workspace_id = $1 AND id = $2",
        )
        .bind(world.workspace_id)
        .bind(target_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        row
    };
    tx.commit().await.unwrap();

    let freeze_request = FreezeMessageDraftRequest {
        delivery_channel_id: channel_id,
        sender_identity_id: Some(sender_id),
        meeting_type_id: None,
        tracking_requested: false,
    };
    let tx = api_db.begin(world.workspace_id).await.unwrap();
    let (tx, frozen) =
        sending::freeze_draft_in_tx(tx, &admin, draft_id, draft.version, &freeze_request)
            .await
            .unwrap();
    let outbound_id = frozen.outbound_message_id;
    let payload_hash = frozen.payload_hash;
    tx.commit().await.unwrap();

    // Queueing a campaign card is the go-ahead: no approval request/decision.
    let mut tx = api_db.begin(world.workspace_id).await.unwrap();
    let queued = sales_campaigns::queue_sales_campaign_target(
        &mut tx,
        &admin,
        campaign_id,
        target_id,
        &QueueSalesCampaignTargetRequest {
            outbound_message_id: outbound_id,
            expected_payload_hash: payload_hash.clone(),
            expected_target_version: target_version,
        },
    )
    .await
    .unwrap();
    let batch_at = queued.batch_at;
    tx.commit().await.unwrap();
    (campaign_id, target_id, outbound_id, batch_at, payload_hash)
}

async fn dispatch_after_due(
    owner_db: &Db,
    runner: &Runner,
    world: &TestWorld,
    job_id: Uuid,
    outbound_id: Uuid,
) -> usize {
    let due = Utc::now() - Duration::seconds(1);
    let mut tx = owner_db.begin(world.workspace_id).await.unwrap();
    sqlx::query(
        "UPDATE outbound_messages SET not_before = $3
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(world.workspace_id)
    .bind(outbound_id)
    .bind(due)
    .execute(tx.conn())
    .await
    .unwrap();
    sqlx::query("UPDATE jobs SET available_at = $3 WHERE workspace_id = $1 AND id = $2")
        .bind(world.workspace_id)
        .bind(job_id)
        .bind(due)
        .execute(tx.conn())
        .await
        .unwrap();
    tx.commit().await.unwrap();

    runner.run_workspace_once(world.workspace_id).await.unwrap()
}

#[tokio::test]
async fn queued_cards_wait_for_the_batch_then_follow_fake_provider_acceptance_or_unknown() {
    let Some((api_db, worker_db, world)) = test_databases().await else {
        return;
    };
    let owner_db = Db::connect(&std::env::var("DATABASE_URL").unwrap(), None)
        .await
        .unwrap();

    for (suffix, outcome, expected_status, expected_stage) in [
        (
            "accepted",
            SendOutcome::Accepted {
                provider_message_id: "fake-accepted".into(),
            },
            "accepted",
            "accepted",
        ),
        (
            "unknown",
            SendOutcome::Unknown {
                reason: "fake provider outcome unknown".into(),
            },
            "unknown",
            "attention",
        ),
    ] {
        let (campaign_id, target_id, outbound_id, batch_at, _hash) =
            queued_sales_message(&api_db, &world, suffix).await;
        assert!(batch_at > Utc::now());
        assert_eq!(batch_at.timestamp().rem_euclid(600), 0);

        let fake_delivery = Arc::new(RecordingDelivery::with_outcome(outcome));
        let fake_mail = Arc::new(FakeMailProvider::new());
        let context = WorkerContext {
            db: worker_db.clone(),
            config: Arc::new(Config::test()),
            mail: fake_mail.clone() as Arc<dyn MailProvider>,
            delivery: fake_delivery.clone() as Arc<dyn DeliveryProvider>,
            calendar: fake_mail as Arc<dyn infrastructure::providers::CalendarProvider>,
        };
        let mut registry = JobRegistry::new();
        worker::handlers::sending::register_with_context(&mut registry, &context);
        let runner = Runner::new(worker_db.clone(), registry);
        assert_eq!(
            runner.run_workspace_once(world.workspace_id).await.unwrap(),
            0
        );
        assert_eq!(
            fake_delivery.send_count(),
            0,
            "nothing sends before batch_at"
        );

        let mut tx = worker_db.begin(world.workspace_id).await.unwrap();
        let status: String = sqlx::query_scalar(
            "SELECT status FROM outbound_messages WHERE workspace_id = $1 AND id = $2",
        )
        .bind(world.workspace_id)
        .bind(outbound_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        assert_eq!(status, "queued");
        let job_id: Uuid = sqlx::query_scalar(
            "SELECT id FROM jobs WHERE workspace_id = $1 AND kind = $2
              AND payload->>'outbound_message_id' = $3",
        )
        .bind(world.workspace_id)
        .bind(sending::dispatch::DISPATCH_KIND)
        .bind(outbound_id.to_string())
        .fetch_one(tx.conn())
        .await
        .unwrap();
        tx.rollback().await.unwrap();

        assert_eq!(
            dispatch_after_due(&owner_db, &runner, &world, job_id, outbound_id,).await,
            1
        );
        assert_eq!(fake_delivery.send_count(), 1);

        let detail = sales_campaigns::get_sales_campaign(
            &api_db,
            &ctx_for(&world, &world.admin, "sales-kanban-board-after-send"),
            campaign_id,
        )
        .await
        .unwrap();
        let target = detail
            .targets
            .iter()
            .find(|target| target.id == target_id)
            .unwrap();
        assert_eq!(target.outbound.as_ref().unwrap().status, expected_status);
        assert_eq!(target.stage, expected_stage);
        assert_eq!(
            target.outbound.as_ref().unwrap().provider_accepted,
            expected_status == "accepted"
        );

        if expected_status == "unknown" {
            let mut tx = worker_db.begin(world.workspace_id).await.unwrap();
            let retry_dispatch_jobs: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM jobs WHERE workspace_id = $1 AND kind = $2
                  AND payload->>'outbound_message_id' = $3 AND state IN ('queued','retry_wait')",
            )
            .bind(world.workspace_id)
            .bind(sending::dispatch::DISPATCH_KIND)
            .bind(outbound_id.to_string())
            .fetch_one(tx.conn())
            .await
            .unwrap();
            assert_eq!(
                retry_dispatch_jobs, 0,
                "unknown outcomes are not auto-retried"
            );
            tx.rollback().await.unwrap();
        }
    }
}
