//! Exact outbound reply attribution. Tests use the dedicated
//! `outreach_reply_verify` database through `DATABASE_URL`.

mod common;

use application::inbox::ingest::ingest_provider_message;
use application::sending;
use application::sending::dispatch::{DISPATCH_KIND, Providers, run_dispatch};
use chrono::{Duration, Utc};
use common::{seed_db, test_db};
use contracts::AuthMethod;
use contracts::approvals::CreateDecisionRequest;
use contracts::sending::{
    CreateMessageDraftRequest, CreateOutboundApprovalRequest, FreezeMessageDraftRequest,
    SubmitOutboundRequest,
};
use contracts::smtp::CreateSmtpConnectionRequest;
use contracts::{Scope, ScopeSet};
use infrastructure::config::Config;
use infrastructure::db::{Db, Tx};
use infrastructure::jobs::{self, JobCtx};
use infrastructure::providers::fake::FakeMailProvider;
use infrastructure::providers::{
    DeliveryCapabilities, DeliveryProvider, ProviderError, ProviderMessage, SendOutcome,
    SendRequest, SentLookup,
};
use infrastructure::repo::inbox as repo;
use infrastructure::secrets::{FileSecretStore, SecretStore};
use infrastructure::testing::{TestWorld, seed_workspace};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

struct SentFixture {
    outbound_id: Uuid,
    thread_id: Uuid,
    internet_message_id: String,
}

async fn insert_address(tx: &mut Tx<'_>, address: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO email_addresses (workspace_id, address_original, normalized_key)
         VALUES ($1, $2, $2)
         ON CONFLICT (workspace_id, normalized_key)
         DO UPDATE SET last_seen_at = now()
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(address)
    .fetch_one(tx.conn())
    .await
    .unwrap()
}

async fn insert_mailbox_b(tx: &mut Tx<'_>, world: &TestWorld) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO mail_accounts
            (workspace_id, connection_id, provider_account_id, primary_address, name, status)
         VALUES ($1, $2, $3, $4, 'Reply Fixture B', 'active') RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(world.connection_id)
    .bind(format!("reply-fixture-{}", Uuid::new_v4()))
    .bind(format!("reply-fixture-{}@test.dev", Uuid::new_v4()))
    .fetch_one(tx.conn())
    .await
    .unwrap()
}

async fn create_correspondence_smtp_profile(
    fixtures: &Db,
    world: &TestWorld,
    config: &Config,
) -> (Uuid, Uuid) {
    let session_id = Uuid::new_v4();
    let session_hash = infrastructure::crypto::sha256_bytes(session_id.as_bytes());
    let mut tx = fixtures.begin(world.workspace_id).await.unwrap();
    sqlx::query(
        "INSERT INTO user_sessions (id, workspace_id, principal_id, session_hash, expires_at)
         VALUES ($1, $2, $3, $4, now() + interval '1 day')",
    )
    .bind(session_id)
    .bind(world.workspace_id)
    .bind(world.admin.principal_id)
    .bind(&session_hash[..])
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mut ctx = infrastructure::testing::ctx_for(world, &world.admin, "smtp-profile-create");
    ctx.auth = AuthMethod::Session { session_id };
    let request = CreateSmtpConnectionRequest {
        name: "Correspondence SMTP fixture".into(),
        mail_account_id: world.mail_account_id,
        purpose: Some("correspondence".into()),
        host: "smtp.example.test".into(),
        port: 587,
        security: "starttls".into(),
        username: "reply-fixture".into(),
        password: format!("fake-smtp-{}", Uuid::new_v4()),
        from_address: "reply-sender@test.dev".into(),
        from_name: "Team Replies".into(),
        reply_to: Some("support@test.dev".into()),
        custom_headers: Default::default(),
        sender_confirmed: true,
    };
    let mut tx = fixtures.begin(world.workspace_id).await.unwrap();
    let profile = application::smtp::create_tx(&mut tx, &ctx, config, Uuid::new_v4(), &request)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(profile.purpose, "correspondence");

    let mut tx = fixtures.begin(world.workspace_id).await.unwrap();
    let active = infrastructure::repo::smtp::activate_verified(&mut tx, profile.id)
        .await
        .unwrap()
        .expect("allowed correspondence profile with a password can activate");
    assert_eq!(active.status, "active");
    assert_eq!(active.purpose, "correspondence");
    tx.commit().await.unwrap();

    let profile_row = {
        let mut tx = fixtures.begin(world.workspace_id).await.unwrap();
        let row = infrastructure::repo::smtp::get_for_update(&mut tx, profile.id)
            .await
            .unwrap()
            .unwrap();
        tx.rollback().await.unwrap();
        row
    };
    let store = FileSecretStore::new(
        &config.secrets_file,
        config.secret_store_key.expect("test secret key"),
    );
    assert!(
        store.get(&profile_row.secret_ref).await.unwrap().is_some(),
        "the profile stores a valid encrypted test credential"
    );
    (profile.id, profile.channel_id)
}

async fn create_agent_compose_credential(fixtures: &Db, world: &TestWorld) -> Uuid {
    let credential_id = Uuid::new_v4();
    let token_hash = infrastructure::crypto::sha256_bytes(Uuid::new_v4().as_bytes());
    let mut tx = fixtures.begin(world.workspace_id).await.unwrap();
    sqlx::query(
        "INSERT INTO service_credentials
            (id, workspace_id, principal_id, name, key_prefix, token_hash, scopes, expires_at)
         VALUES ($1, $2, $3, 'Reply fixture agent', $4, $5, $6, now() + interval '1 day')",
    )
    .bind(credential_id)
    .bind(world.workspace_id)
    .bind(world.agent.principal_id)
    .bind(format!("reply-{}", credential_id.simple()))
    .bind(&token_hash[..])
    .bind(vec![Scope::MailCompose.as_str().to_owned()])
    .execute(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO mail_account_grants
            (workspace_id, mail_account_id, principal_id, can_read, can_compose,
             can_send, can_approve, can_assign, granted_by)
         VALUES ($1, $2, $3, true, true, true, false, false, $4)",
    )
    .bind(world.workspace_id)
    .bind(world.mail_account_id)
    .bind(world.agent.principal_id)
    .bind(world.admin.principal_id)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    credential_id
}

#[derive(Clone)]
struct CapturingSmtp {
    workspace_id: Uuid,
    channel_id: Uuid,
    sends: Arc<Mutex<Vec<SendRequest>>>,
}

impl CapturingSmtp {
    fn new(workspace_id: Uuid, channel_id: Uuid) -> Self {
        Self {
            workspace_id,
            channel_id,
            sends: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn sent(&self) -> Vec<SendRequest> {
        self.sends.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl DeliveryProvider for CapturingSmtp {
    async fn for_channel(
        &self,
        workspace_id: Uuid,
        channel_id: Uuid,
    ) -> Result<Option<Arc<dyn DeliveryProvider>>, ProviderError> {
        if workspace_id == self.workspace_id && channel_id == self.channel_id {
            Ok(Some(Arc::new(self.clone())))
        } else {
            Ok(None)
        }
    }

    fn capabilities(&self) -> DeliveryCapabilities {
        DeliveryCapabilities {
            supports_send: true,
            custom_headers: true,
            ..Default::default()
        }
    }

    async fn send(&self, request: SendRequest) -> Result<SendOutcome, ProviderError> {
        self.sends.lock().unwrap().push(request.clone());
        Ok(SendOutcome::Accepted {
            provider_message_id: request
                .internet_message_id
                .unwrap_or_else(|| "fake-smtp-accepted".into()),
        })
    }

    async fn lookup_sent(
        &self,
        _correlation_id: &str,
    ) -> Result<Option<SentLookup>, ProviderError> {
        Ok(None)
    }
}

async fn insert_sent_fixture(
    tx: &mut Tx<'_>,
    world: &TestWorld,
    mail_account_id: Uuid,
    recipient: &str,
    message_id: &str,
    sent_at: chrono::DateTime<Utc>,
    mirror_outbound: bool,
) -> SentFixture {
    let ws = tx.workspace_id().unwrap();
    let thread_id = repo::create_thread(tx, mail_account_id, None, None)
        .await
        .unwrap();
    let from_address = format!("sender-{}@test.dev", Uuid::new_v4().simple());
    let sender_identity_id: Uuid = sqlx::query_scalar(
        "INSERT INTO sender_identities (workspace_id, mail_account_id, address, display_name, status)
         VALUES ($1, $2, $3, 'Shared', 'verified') RETURNING id",
    )
    .bind(ws)
    .bind(mail_account_id)
    .bind(&from_address)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let delivery_channel_id: Uuid = sqlx::query_scalar(
        "INSERT INTO delivery_channels
            (workspace_id, mail_account_id, sender_identity_id, connection_id,
             provider, purpose, name)
         VALUES ($1, $2, $3, $4, 'zoho_mail', 'marketing', 'Reply fixture')
         RETURNING id",
    )
    .bind(ws)
    .bind(mail_account_id)
    .bind(sender_identity_id)
    .bind(world.connection_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let recipient_id = insert_address(tx, recipient).await;
    let outbound_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO outbound_messages
            (id, workspace_id, mail_account_id, delivery_channel_id, sender_identity_id,
             to_email_address_id, thread_id, requested_by, purpose, from_address_snapshot,
             to_address_snapshot, subject, body_text, payload_hash, internet_message_id,
             status, accepted_at, provider_message_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'marketing', $9,
                 $10, 'Same subject', 'sent body', decode(md5(random()::text), 'hex'),
                 $11, 'accepted', $12, $13)",
    )
    .bind(outbound_id)
    .bind(ws)
    .bind(mail_account_id)
    .bind(delivery_channel_id)
    .bind(sender_identity_id)
    .bind(recipient_id)
    .bind(thread_id)
    .bind(world.admin.principal_id)
    .bind(&from_address)
    .bind(recipient)
    .bind(message_id)
    .bind(sent_at)
    .bind(format!("smtp-{outbound_id}"))
    .execute(tx.conn())
    .await
    .unwrap();
    if mirror_outbound {
        sqlx::query(
            "INSERT INTO email_messages
            (workspace_id, mail_account_id, thread_id, outbound_message_id, direction,
             internet_message_id, subject, message_at, classification)
         VALUES ($1, $2, $3, $4, 'outbound', $5, 'Same subject', $6, 'unknown')",
        )
        .bind(ws)
        .bind(mail_account_id)
        .bind(thread_id)
        .bind(outbound_id)
        .bind(message_id)
        .bind(sent_at)
        .execute(tx.conn())
        .await
        .unwrap();
    }

    SentFixture {
        outbound_id,
        thread_id,
        internet_message_id: message_id.to_owned(),
    }
}

fn reply(
    provider_message_id: &str,
    from: &str,
    provider_thread_id: Option<&str>,
    in_reply_to: Option<&str>,
    references: Vec<&str>,
    subject: &str,
) -> ProviderMessage {
    ProviderMessage {
        provider_message_id: provider_message_id.to_owned(),
        provider_thread_id: provider_thread_id.map(str::to_owned),
        internet_message_id: Some(format!("<{provider_message_id}@reply.example.test>")),
        in_reply_to: in_reply_to.map(str::to_owned),
        references: references.into_iter().map(str::to_owned).collect(),
        from: from.to_owned(),
        to: vec!["shared@test.dev".into()],
        subject: Some(subject.to_owned()),
        body_text: Some("Thanks for reaching out.".into()),
        body_html: None,
        message_at: Utc::now(),
    }
}

#[tokio::test]
async fn sent_mail_reply_matches_its_message_id_and_sender() {
    let Some(db) = test_db().await else { return };
    let fixtures = seed_db().await;
    let world = seed_workspace(&fixtures).await.unwrap();
    let sent = {
        let mut tx = fixtures.begin(world.workspace_id).await.unwrap();
        let sent = insert_sent_fixture(
            &mut tx,
            &world,
            world.mail_account_id,
            "lead@example.test",
            "<sent-1@office.example.test>",
            Utc::now(),
            true,
        )
        .await;
        tx.commit().await.unwrap();
        sent
    };

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let outcome = ingest_provider_message(
        &mut tx,
        None,
        world.mail_account_id,
        &reply(
            "reply-1",
            "LEAD@example.test",
            None,
            Some(&sent.internet_message_id),
            vec!["<root@office.example.test>"],
            "Re: Same subject",
        ),
    )
    .await
    .unwrap();

    assert_eq!(outcome.thread_id, sent.thread_id);
    let inbound_message_id = outcome.message_id.expect("message is ingested");
    let linked = repo::list_outbound_replies(&mut tx, sent.outbound_id)
        .await
        .unwrap();
    assert_eq!(linked.len(), 1);
    assert_eq!(linked[0].inbound_message_id, inbound_message_id);
    assert_eq!(linked[0].thread_id, sent.thread_id);
    assert_eq!(linked[0].from_address, "LEAD@example.test");
    let reply_headers =
        repo::reply_headers_for_message(&mut tx, world.mail_account_id, inbound_message_id)
            .await
            .unwrap()
            .expect("reply has a usable RFC Message-ID");
    assert_eq!(reply_headers.in_reply_to, "<reply-1@reply.example.test>");
    assert_eq!(
        reply_headers.references,
        vec!["<root@office.example.test>", "<reply-1@reply.example.test>"]
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn sent_outbound_projection_is_idempotent_and_keeps_the_frozen_message_id() {
    let Some(db) = test_db().await else { return };
    let fixtures = seed_db().await;
    let world = seed_workspace(&fixtures).await.unwrap();
    let sent = {
        let mut tx = fixtures.begin(world.workspace_id).await.unwrap();
        let sent = insert_sent_fixture(
            &mut tx,
            &world,
            world.mail_account_id,
            "lead@example.test",
            "<stored-outbound@office.example.test>",
            Utc::now(),
            false,
        )
        .await;
        tx.commit().await.unwrap();
        sent
    };

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let first = repo::record_sent_outbound(&mut tx, sent.outbound_id)
        .await
        .unwrap()
        .expect("accepted outbound is projected into its thread");
    let second = repo::record_sent_outbound(&mut tx, sent.outbound_id)
        .await
        .unwrap()
        .expect("replaying the result returns the same projection");
    assert_eq!(first, second);
    let message = repo::get_message(&mut tx, first).await.unwrap().unwrap();
    assert_eq!(message.direction, "outbound");
    assert_eq!(
        message.internet_message_id.as_deref(),
        Some("<stored-outbound@office.example.test>")
    );
    assert_eq!(message.thread_id, sent.thread_id);
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn references_chain_prefers_in_reply_to_over_newer_ancestor() {
    let Some(db) = test_db().await else { return };
    let fixtures = seed_db().await;
    let world = seed_workspace(&fixtures).await.unwrap();
    let (older_target, newer_ancestor) = {
        let mut tx = fixtures.begin(world.workspace_id).await.unwrap();
        let older_target = insert_sent_fixture(
            &mut tx,
            &world,
            world.mail_account_id,
            "lead@example.test",
            "<older-target@office.example.test>",
            Utc::now() - Duration::days(3),
            true,
        )
        .await;
        let newer_ancestor = insert_sent_fixture(
            &mut tx,
            &world,
            world.mail_account_id,
            "lead@example.test",
            "<newer-ancestor@office.example.test>",
            Utc::now() - Duration::days(1),
            true,
        )
        .await;
        tx.commit().await.unwrap();
        (older_target, newer_ancestor)
    };

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let outcome = ingest_provider_message(
        &mut tx,
        None,
        world.mail_account_id,
        &reply(
            "reply-chain",
            "lead@example.test",
            None,
            Some(&older_target.internet_message_id),
            vec![
                "<older-target@office.example.test>",
                "<newer-ancestor@office.example.test>",
            ],
            "Unrelated subject is not a thread key",
        ),
    )
    .await
    .unwrap();

    assert_eq!(outcome.thread_id, older_target.thread_id);
    assert_ne!(outcome.thread_id, newer_ancestor.thread_id);
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn multi_message_references_can_match_without_in_reply_to() {
    let Some(db) = test_db().await else { return };
    let fixtures = seed_db().await;
    let world = seed_workspace(&fixtures).await.unwrap();
    let sent = {
        let mut tx = fixtures.begin(world.workspace_id).await.unwrap();
        let sent = insert_sent_fixture(
            &mut tx,
            &world,
            world.mail_account_id,
            "lead@example.test",
            "<references-target@office.example.test>",
            Utc::now(),
            true,
        )
        .await;
        tx.commit().await.unwrap();
        sent
    };

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let outcome = ingest_provider_message(
        &mut tx,
        None,
        world.mail_account_id,
        &reply(
            "references-only-reply",
            "lead@example.test",
            None,
            None,
            vec![
                "<unknown-root@office.example.test>",
                "<references-target@office.example.test>",
            ],
            "Re: unrelated subject",
        ),
    )
    .await
    .unwrap();

    assert_eq!(outcome.thread_id, sent.thread_id);
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn forged_sender_and_provider_thread_cannot_attach_to_outbound() {
    let Some(db) = test_db().await else { return };
    let fixtures = seed_db().await;
    let world = seed_workspace(&fixtures).await.unwrap();
    let (sent, colliding_provider_thread) = {
        let mut tx = fixtures.begin(world.workspace_id).await.unwrap();
        let sent = insert_sent_fixture(
            &mut tx,
            &world,
            world.mail_account_id,
            "lead@example.test",
            "<forged-target@office.example.test>",
            Utc::now(),
            true,
        )
        .await;
        let collision = repo::create_thread(
            &mut tx,
            world.mail_account_id,
            Some("provider-thread-from-unrelated-message"),
            None,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        (sent, collision)
    };

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let outcome = ingest_provider_message(
        &mut tx,
        None,
        world.mail_account_id,
        &reply(
            "forged-reply",
            "attacker@example.test",
            Some("provider-thread-from-unrelated-message"),
            Some(&sent.internet_message_id),
            vec![],
            "Re: Same subject",
        ),
    )
    .await
    .unwrap();

    assert_ne!(outcome.thread_id, sent.thread_id);
    assert_ne!(outcome.thread_id, colliding_provider_thread);
    assert!(
        repo::list_outbound_replies(&mut tx, sent.outbound_id)
            .await
            .unwrap()
            .is_empty()
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn message_id_from_another_mailbox_does_not_match() {
    let Some(db) = test_db().await else { return };
    let fixtures = seed_db().await;
    let world = seed_workspace(&fixtures).await.unwrap();
    let (mailbox_b, sent_b) = {
        let mut tx = fixtures.begin(world.workspace_id).await.unwrap();
        let mailbox_b = insert_mailbox_b(&mut tx, &world).await;
        let sent = insert_sent_fixture(
            &mut tx,
            &world,
            mailbox_b,
            "lead@example.test",
            "<same-workspace-mailbox-b@office.example.test>",
            Utc::now(),
            true,
        )
        .await;
        tx.commit().await.unwrap();
        (mailbox_b, sent)
    };

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let outcome = ingest_provider_message(
        &mut tx,
        None,
        world.mail_account_id,
        &reply(
            "foreign-mailbox-reply",
            "lead@example.test",
            None,
            Some(&sent_b.internet_message_id),
            vec![],
            "Re: Same subject",
        ),
    )
    .await
    .unwrap();

    assert_ne!(outcome.thread_id, sent_b.thread_id);
    let local_thread = repo::get_thread(&mut tx, outcome.thread_id)
        .await
        .unwrap()
        .expect("reply gets a local thread");
    assert_eq!(local_thread.mail_account_id, world.mail_account_id);
    assert_ne!(local_thread.mail_account_id, mailbox_b);
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn identical_subject_without_message_id_starts_a_new_thread() {
    let Some(db) = test_db().await else { return };
    let fixtures = seed_db().await;
    let world = seed_workspace(&fixtures).await.unwrap();
    let sent = {
        let mut tx = fixtures.begin(world.workspace_id).await.unwrap();
        let sent = insert_sent_fixture(
            &mut tx,
            &world,
            world.mail_account_id,
            "lead@example.test",
            "<subject-only@office.example.test>",
            Utc::now(),
            true,
        )
        .await;
        tx.commit().await.unwrap();
        sent
    };

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let outcome = ingest_provider_message(
        &mut tx,
        None,
        world.mail_account_id,
        &reply(
            "subject-only-reply",
            "lead@example.test",
            None,
            None,
            vec![],
            "Same subject",
        ),
    )
    .await
    .unwrap();

    assert_ne!(outcome.thread_id, sent.thread_id);
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn approved_agent_reply_dispatches_through_a_correspondence_smtp_profile() {
    let Some(db) = test_db().await else { return };
    let fixtures = seed_db().await;
    let world = seed_workspace(&fixtures).await.unwrap();
    let mut config = Config::test();
    config.secrets_file = std::env::temp_dir().join(format!(
        "outreach-reply-smtp-{}.json",
        Uuid::new_v4().simple()
    ));

    let mut tx = fixtures.begin(world.workspace_id).await.unwrap();
    sqlx::query("UPDATE workspaces SET emergency_stop = false WHERE id = $1")
        .bind(world.workspace_id)
        .execute(tx.conn())
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let (_profile_id, channel_id) =
        create_correspondence_smtp_profile(&fixtures, &world, &config).await;
    let credential_id = create_agent_compose_credential(&fixtures, &world).await;
    let agent_ctx = infrastructure::testing::ctx_for_credential(
        &world,
        &world.agent,
        credential_id,
        ScopeSet::of([Scope::MailCompose]),
        "agent-correspondence-reply",
    );
    let manager_ctx = infrastructure::testing::ctx_for(&world, &world.manager, "human-approval");

    let inbound_id = Uuid::new_v4();
    let inbound = ProviderMessage {
        provider_message_id: format!("smtp-reply-inbound-{inbound_id}"),
        provider_thread_id: None,
        internet_message_id: Some(format!("<inbound-{inbound_id}@customer.example.test>")),
        in_reply_to: None,
        references: vec!["<root@customer.example.test>".into()],
        from: "Lead@customer.example.test".into(),
        to: vec!["shared@test.dev".into()],
        subject: Some("Could we discuss this?".into()),
        body_text: Some("Please send more information.".into()),
        body_html: None,
        message_at: Utc::now(),
    };
    let inbound_outcome = {
        let mut tx = db.begin(world.workspace_id).await.unwrap();
        let outcome = ingest_provider_message(&mut tx, None, world.mail_account_id, &inbound)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        outcome
    };
    let inbound_message_id = inbound_outcome
        .message_id
        .expect("customer message is ingested");
    let recipient_id = {
        let mut tx = db.begin(world.workspace_id).await.unwrap();
        let address = repo::from_address_id(&mut tx, inbound_message_id)
            .await
            .unwrap()
            .expect("reply recipient is the inbound From address");
        tx.rollback().await.unwrap();
        address
    };

    let draft = sending::create_draft(
        &db,
        &agent_ctx,
        &CreateMessageDraftRequest {
            mail_account_id: world.mail_account_id,
            to_email_address_id: recipient_id,
            purpose: "correspondence".into(),
            subject: "Re: Could we discuss this?".into(),
            body_text: "Thank you. Here is more information.".into(),
            body_html: None,
            template_version_id: None,
            variables: None,
            thread_id: Some(inbound_outcome.thread_id),
            reply_to_message_id: Some(inbound_message_id),
            expected_last_inbound_id: Some(inbound_message_id),
            visibility: Some("mailbox".into()),
        },
    )
    .await
    .unwrap();
    let frozen = sending::freeze_draft(
        &db,
        &agent_ctx,
        draft.draft_id,
        draft.version,
        &FreezeMessageDraftRequest {
            delivery_channel_id: channel_id,
            sender_identity_id: None,
            meeting_type_id: None,
            tracking_requested: false,
        },
    )
    .await
    .unwrap();
    let approval = sending::create_approval_request(
        &db,
        &agent_ctx,
        frozen.outbound_message_id,
        &CreateOutboundApprovalRequest {
            reason: "review reply to customer request".into(),
            expected_payload_hash: frozen.payload_hash.clone(),
            expires_at: None,
        },
    )
    .await
    .unwrap();
    let decision = {
        let mut tx = db.begin(world.workspace_id).await.unwrap();
        let decision = application::approvals::decide(
            &mut tx,
            &manager_ctx,
            approval.approval_request_id,
            approval.version,
            &CreateDecisionRequest {
                decision: "approved".into(),
                expected_snapshot_hash: frozen.payload_hash.clone(),
                reason: Some("approved reply".into()),
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        decision
    };
    assert_eq!(decision.approval_status, "approved");
    assert_eq!(decision.execution_status, "ready");
    let submitted = sending::submit(
        &db,
        &manager_ctx,
        frozen.outbound_message_id,
        &SubmitOutboundRequest {
            expected_payload_hash: frozen.payload_hash.clone(),
            not_before: None,
        },
        "approved-correspondence-reply",
    )
    .await
    .unwrap()
    .body;

    let fake_smtp = CapturingSmtp::new(world.workspace_id, channel_id);
    let providers = Providers {
        mail: Arc::new(FakeMailProvider::new()),
        delivery: Arc::new(fake_smtp.clone()),
    };
    let claim = {
        let mut tx = db.begin(world.workspace_id).await.unwrap();
        let jobs = jobs::claim(&mut tx, "reply-smtp-test", 10, &[DISPATCH_KIND])
            .await
            .unwrap();
        tx.commit().await.unwrap();
        jobs.into_iter()
            .find(|job| job.id == submitted.job_id)
            .expect("approved submit queued one dispatch job")
    };
    let job_ctx = JobCtx {
        job_id: claim.id,
        workspace_id: world.workspace_id,
        lease_token: claim.lease_token.expect("claimed dispatch lease"),
        attempt_count: claim.attempt_count,
        lease_owner: "reply-smtp-test".into(),
    };
    run_dispatch(&db, &providers, &job_ctx, frozen.outbound_message_id)
        .await
        .unwrap();
    {
        let mut tx = db.begin(world.workspace_id).await.unwrap();
        jobs::complete(&mut tx, claim.id, job_ctx.lease_token)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    let sent = fake_smtp.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].to, "Lead@customer.example.test");
    assert_eq!(sent[0].from, "reply-sender@test.dev");
    let inbound_message_id = inbound.internet_message_id.clone().unwrap();
    assert_eq!(
        sent[0].in_reply_to.as_deref(),
        Some(inbound_message_id.as_str())
    );
    assert_eq!(
        sent[0].references,
        vec![
            "<root@customer.example.test>".to_owned(),
            inbound_message_id
        ]
    );
    assert!(sent[0].internet_message_id.is_some());

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let outbound = infrastructure::repo::sending::get_outbound(&mut tx, frozen.outbound_message_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(outbound.status, "accepted");
    assert_eq!(outbound.thread_id, Some(inbound_outcome.thread_id));
    assert!(outbound.internet_message_id.is_some());
    let thread_messages = repo::thread_messages(&mut tx, inbound_outcome.thread_id)
        .await
        .unwrap();
    assert!(thread_messages.iter().any(|message| {
        message.direction == "outbound"
            && message.outbound_message_id == Some(frozen.outbound_message_id)
    }));
    tx.rollback().await.unwrap();
    let _ = std::fs::remove_file(&config.secrets_file);
}
