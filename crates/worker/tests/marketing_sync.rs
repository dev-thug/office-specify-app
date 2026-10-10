//! Loopback Campaigns and dedicated-database tests for marketing sync jobs.

mod common;

use std::{
    collections::VecDeque,
    io,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use chrono::{Duration, Utc};
use contracts::Ctx;
use domain::marketing::MarketingExportState;
use infrastructure::{
    config::Config,
    db::Db,
    jobs,
    providers::zoho_campaigns::{
        CampaignsOAuthConfig, SecretValue,
        http::{CampaignsHttp, OAuthTokens},
        operations::CampaignsOperations,
    },
    repo::{campaigns as campaign_repo, compliance_core, marketing, marketing_exports},
    secrets::{FileSecretStore, SecretStore},
    testing::seed_workspace,
};
use uuid::Uuid;
use worker::{JobRegistry, Runner, handlers::marketing as marketing_jobs};

#[derive(Clone)]
struct MockReply {
    status: u16,
    body: String,
}

#[derive(Clone, Debug)]
struct CapturedRequest {
    method: String,
    path: String,
    authorization: Option<String>,
    body: String,
}

struct MockServer {
    base_url: String,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
    task: tokio::task::JoinHandle<()>,
}

async fn mock_campaigns_server(replies: Vec<MockReply>) -> MockServer {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let queue = Arc::new(Mutex::new(VecDeque::from(replies)));
    let capture = requests.clone();
    let task = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let requests = capture.clone();
            let replies = queue.clone();
            tokio::spawn(async move {
                if let Err(error) = serve_one(stream, requests, replies).await {
                    tracing::debug!(%error, "Campaigns loopback mock ended a request");
                }
            });
        }
    });
    MockServer {
        base_url,
        requests,
        task,
    }
}

async fn serve_one(
    mut stream: tokio::net::TcpStream,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
    replies: Arc<Mutex<VecDeque<MockReply>>>,
) -> io::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4096];
    let header_end = loop {
        let count = stream.read(&mut chunk).await?;
        if count == 0 {
            return Ok(());
        }
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let header = String::from_utf8_lossy(&bytes[..end]);
            let content_length = header
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if bytes.len() >= end + 4 + content_length {
                break end;
            }
        }
        if bytes.len() > 2 * 1024 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "mock request exceeded its test limit",
            ));
        }
    };
    let header = String::from_utf8_lossy(&bytes[..header_end]);
    let mut lines = header.lines();
    let request_line = lines.next().unwrap_or_default();
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts.next().unwrap_or_default().to_owned();
    let path = request_parts.next().unwrap_or_default().to_owned();
    let authorization = lines.find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("authorization")
            .then(|| value.trim().to_owned())
    });
    let body_start = header_end + 4;
    let body = String::from_utf8_lossy(&bytes[body_start..]).to_string();
    requests.lock().unwrap().push(CapturedRequest {
        method,
        path,
        authorization,
        body,
    });
    let reply = replies.lock().unwrap().pop_front().unwrap_or(MockReply {
        status: 500,
        body: "{}".into(),
    });
    let response = format!(
        "HTTP/1.1 {} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        reply.status,
        reply.body.len(),
        reply.body
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

struct SecretPath(PathBuf);
impl Drop for SecretPath {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_file(self.0.with_extension("lock"));
    }
}

struct CampaignsFixture {
    api_db: Db,
    worker_db: Db,
    workspace_id: Uuid,
    system_principal_id: Uuid,
    mail_account_id: Uuid,
    email_address_id: Uuid,
    normalized_email: String,
    connection_id: Uuid,
    admin_ctx: Ctx,
    member_ctx: Ctx,
    config: Config,
    _secret_path: SecretPath,
}

async fn fixture(email: &str) -> CampaignsFixture {
    let worker_db = common::test_db().await.expect("dedicated worker DB");
    let api_db = common::seed_db().await;
    let world = seed_workspace(&api_db).await.unwrap();
    let admin_ctx = infrastructure::testing::ctx_for(&world, &world.admin, "sync-status-admin");
    let member_ctx =
        infrastructure::testing::ctx_for(&world, &world.member_a, "sync-status-member");
    let normalized =
        domain::email_address::normalize(email, domain::email_address::LocalPartPolicy::Lowercase)
            .unwrap();
    let mut tx = api_db.begin(world.workspace_id).await.unwrap();
    let email_address_id: Uuid = sqlx::query_scalar(
        "INSERT INTO email_addresses
            (workspace_id, address_original, normalized_key, kind, verification_status, verified_at)
         VALUES ($1, $2, $3, 'person', 'verified', now())
         RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(&normalized.original)
    .bind(&normalized.normalized_key)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let connection_id = Uuid::new_v4();
    let secret_ref = format!("zoho_campaigns:{}:{connection_id}", world.workspace_id);
    let mut tx = api_db.begin(world.workspace_id).await.unwrap();
    marketing::insert_connection(
        &mut tx,
        connection_id,
        world.admin.principal_id,
        &secret_ref,
        &[
            "ZohoCampaigns.contact.READ".into(),
            "ZohoCampaigns.contact.UPDATE".into(),
            "ZohoCampaigns.campaign.READ".into(),
        ],
    )
    .await
    .unwrap();
    marketing::mark_status(&mut tx, connection_id, "active", None)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let secret_path = PathBuf::from(format!(
        "target/marketing-sync-secrets-{}.json",
        Uuid::new_v4()
    ));
    std::fs::create_dir_all("target").unwrap();
    let mut config = Config::test();
    config.secrets_file = secret_path.clone();
    config.zoho.campaigns = CampaignsOAuthConfig {
        allow_local_redirect: true,
        client_id: Some("1000.test-campaigns-client".into()),
        client_secret: Some(SecretValue::new("test-client-secret")),
        redirect_uri: Some(
            "http://127.0.0.1:3000/api/v1/integrations/zoho-campaigns/callback".into(),
        ),
    };
    let secret_store = FileSecretStore::new(secret_path.clone(), config.secret_store_key.unwrap());
    secret_store
        .put(
            &secret_ref,
            &OAuthTokens {
                access_token: SecretValue::new("loopback-access-token"),
                refresh_token: Some(SecretValue::new("loopback-refresh-token")),
                expires_at: Utc::now() + Duration::hours(2),
            }
            .to_secret_json(),
        )
        .await
        .unwrap();

    CampaignsFixture {
        api_db,
        worker_db,
        workspace_id: world.workspace_id,
        system_principal_id: world.system.principal_id,
        mail_account_id: world.mail_account_id,
        email_address_id,
        normalized_email: normalized.normalized_key,
        connection_id,
        admin_ctx,
        member_ctx,
        config,
        _secret_path: SecretPath(secret_path),
    }
}

async fn add_office_optout(fixture: &CampaignsFixture) -> Uuid {
    let mut tx = fixture.api_db.begin(fixture.workspace_id).await.unwrap();
    assert!(
        compliance_core::lock_email_address(&mut tx, fixture.email_address_id)
            .await
            .unwrap()
    );
    let suppression_id = compliance_core::insert_suppression(
        &mut tx,
        fixture.email_address_id,
        "marketing",
        "unsubscribe",
        "manual",
        Some(fixture.system_principal_id),
        None,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    suppression_id
}

async fn issue_public_unsubscribe_token(fixture: &CampaignsFixture) -> String {
    let mut tx = fixture.api_db.begin(fixture.workspace_id).await.unwrap();
    let sender_identity_id: Uuid = sqlx::query_scalar(
        "INSERT INTO sender_identities
            (workspace_id, mail_account_id, address, display_name, status, verified_at)
         VALUES ($1, $2, 'sender@example.test', 'Office', 'verified', now())
         RETURNING id",
    )
    .bind(fixture.workspace_id)
    .bind(fixture.mail_account_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let delivery_channel_id: Uuid = sqlx::query_scalar(
        "INSERT INTO delivery_channels
            (workspace_id, mail_account_id, sender_identity_id, connection_id,
             provider, purpose, name, policy_status, enabled)
         VALUES ($1, $2, $3,
                 (SELECT connection_id FROM mail_accounts WHERE workspace_id=$1 AND id=$2),
                 'zoho_mail', 'marketing', 'Office marketing', 'allowed', false)
         RETURNING id",
    )
    .bind(fixture.workspace_id)
    .bind(fixture.mail_account_id)
    .bind(sender_identity_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let to_address: String = sqlx::query_scalar(
        "SELECT address_original FROM email_addresses WHERE workspace_id=$1 AND id=$2",
    )
    .bind(fixture.workspace_id)
    .bind(fixture.email_address_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let outbound_message_id: Uuid = sqlx::query_scalar(
        "INSERT INTO outbound_messages
            (workspace_id, mail_account_id, delivery_channel_id, sender_identity_id,
             to_email_address_id, requested_by, purpose, from_address_snapshot,
             to_address_snapshot, subject, body_text, payload_hash, status)
         VALUES ($1, $2, $3, $4, $5, $6, 'marketing', 'sender@example.test',
                 $7, 'hello', 'hello', $8, 'accepted')
         RETURNING id",
    )
    .bind(fixture.workspace_id)
    .bind(fixture.mail_account_id)
    .bind(delivery_channel_id)
    .bind(sender_identity_id)
    .bind(fixture.email_address_id)
    .bind(fixture.admin_ctx.principal_id)
    .bind(to_address)
    .bind(infrastructure::crypto::sha256_bytes(b"unsubscribe test").to_vec())
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let token = compliance_core::issue_unsubscribe_token(
        &mut tx,
        fixture.email_address_id,
        outbound_message_id,
    )
    .await
    .unwrap()
    .token;
    tx.commit().await.unwrap();
    token
}

async fn add_suppressed_address(fixture: &CampaignsFixture, email: &str) -> (Uuid, Uuid) {
    let normalized =
        domain::email_address::normalize(email, domain::email_address::LocalPartPolicy::Lowercase)
            .unwrap();
    let mut tx = fixture.api_db.begin(fixture.workspace_id).await.unwrap();
    let email_address_id: Uuid = sqlx::query_scalar(
        "INSERT INTO email_addresses
            (workspace_id, address_original, normalized_key, kind, verification_status, verified_at)
         VALUES ($1, $2, $3, 'person', 'verified', now())
         RETURNING id",
    )
    .bind(fixture.workspace_id)
    .bind(&normalized.original)
    .bind(&normalized.normalized_key)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let suppression_id = compliance_core::insert_suppression(
        &mut tx,
        email_address_id,
        "marketing",
        "unsubscribe",
        "manual",
        Some(fixture.system_principal_id),
        None,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    (email_address_id, suppression_id)
}

async fn enqueue_push(fixture: &CampaignsFixture, email_address_id: Uuid, suffix: &str) {
    let mut tx = fixture.api_db.begin(fixture.workspace_id).await.unwrap();
    jobs::enqueue(
        &mut tx,
        marketing_jobs::PUSH_SUPPRESSION_KIND,
        1,
        serde_json::json!({"email_address_id": email_address_id}),
        Utc::now(),
        Some(&format!(
            "test_push_suppression:{email_address_id}:{suffix}"
        )),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

fn register(fixture: &CampaignsFixture, base_url: &str, registry: &mut JobRegistry) {
    let http = CampaignsHttp::for_test(fixture.config.zoho.campaigns.clone(), base_url).unwrap();
    let operations = CampaignsOperations::for_test(base_url).unwrap();
    marketing_jobs::register_with_dependencies(
        registry,
        fixture.worker_db.clone(),
        fixture.config.clone(),
        Some(http),
        Some(operations),
    );
}

async fn add_export_snapshot(
    fixture: &CampaignsFixture,
    campaign_key: &str,
    final_state: MarketingExportState,
    extra_recipients: usize,
    expired: bool,
) -> Uuid {
    let mut tx = fixture.api_db.begin(fixture.workspace_id).await.unwrap();
    let sender_identity_id: Uuid = sqlx::query_scalar(
        "INSERT INTO sender_identities
            (workspace_id, mail_account_id, address, display_name, status, verified_at)
         VALUES ($1, $2, $3, 'Campaigns Sync Sender', 'verified', now())
         RETURNING id",
    )
    .bind(fixture.workspace_id)
    .bind(fixture.mail_account_id)
    .bind(format!("sync+{}@example.test", Uuid::new_v4()))
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let mail_account_id = fixture.mail_account_id;
    let channel_id: Uuid = sqlx::query_scalar(
        "INSERT INTO delivery_channels
            (workspace_id, mail_account_id, sender_identity_id, connection_id,
             provider, purpose, name)
         VALUES ($1, $2, $3, $4, 'zoho_campaigns', 'marketing', 'Campaigns Sync')
         RETURNING id",
    )
    .bind(fixture.workspace_id)
    .bind(mail_account_id)
    .bind(sender_identity_id)
    .bind(fixture.connection_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let template_id: Uuid = sqlx::query_scalar(
        "INSERT INTO email_templates (workspace_id, name, created_by)
         VALUES ($1, 'Campaigns sync test', $2) RETURNING id",
    )
    .bind(fixture.workspace_id)
    .bind(fixture.system_principal_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let template_version_id: Uuid = sqlx::query_scalar(
        "INSERT INTO template_versions
            (workspace_id, template_id, version_no, subject, body_text,
             variable_schema, content_hash, created_by)
         VALUES ($1, $2, 1, 'Subject', 'Body', '{}',
                 decode(repeat('11', 32), 'hex'), $3) RETURNING id",
    )
    .bind(fixture.workspace_id)
    .bind(template_id)
    .bind(fixture.system_principal_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let campaign_id = campaign_repo::insert_campaign(
        &mut tx,
        mail_account_id,
        "Campaigns sync test",
        None,
        fixture.system_principal_id,
    )
    .await
    .unwrap();
    let revision_id = campaign_repo::insert_revision(
        &mut tx,
        campaign_id,
        mail_account_id,
        channel_id,
        1,
        "UTC",
        &serde_json::json!({}),
        true,
        true,
        fixture.system_principal_id,
    )
    .await
    .unwrap();
    let step_id = campaign_repo::insert_step(&mut tx, revision_id, template_version_id, 1, 0)
        .await
        .unwrap();
    let expires_at = if expired {
        Utc::now() - Duration::hours(1)
    } else {
        Utc::now() + Duration::days(1)
    };
    let launch_id = campaign_repo::insert_launch(
        &mut tx,
        revision_id,
        channel_id,
        fixture.system_principal_id,
        expires_at,
    )
    .await
    .unwrap();
    let enrollment_id = campaign_repo::insert_enrollment(
        &mut tx,
        launch_id,
        revision_id,
        fixture.email_address_id,
        None,
        "pending",
        None,
    )
    .await
    .unwrap();
    let outbound_id = campaign_repo::insert_step_outbound(
        &mut tx,
        mail_account_id,
        channel_id,
        sender_identity_id,
        fixture.email_address_id,
        None,
        launch_id,
        revision_id,
        enrollment_id,
        step_id,
        fixture.system_principal_id,
        "sender@example.test",
        &fixture.normalized_email,
        "Subject",
        "Body",
        Some("<p>Body</p>"),
        &[0x31; 32],
    )
    .await
    .unwrap();
    let mut export_recipients = vec![marketing_exports::NewExportRecipient {
        email_address_id: fixture.email_address_id,
        outbound_message_id: outbound_id,
        normalized_address: fixture.normalized_email.clone(),
        variables: serde_json::json!({}),
        sync_status: "active".into(),
    }];
    for index in 0..extra_recipients {
        let email = format!("pagination-{index}@example.test");
        let normalized = domain::email_address::normalize(
            &email,
            domain::email_address::LocalPartPolicy::Lowercase,
        )
        .unwrap();
        let email_address_id: Uuid = sqlx::query_scalar(
            "INSERT INTO email_addresses
                (workspace_id, address_original, normalized_key, kind, verification_status, verified_at)
             VALUES ($1, $2, $3, 'person', 'verified', now()) RETURNING id",
        )
        .bind(fixture.workspace_id)
        .bind(&normalized.original)
        .bind(&normalized.normalized_key)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let enrollment_id = campaign_repo::insert_enrollment(
            &mut tx,
            launch_id,
            revision_id,
            email_address_id,
            None,
            "pending",
            None,
        )
        .await
        .unwrap();
        let outbound_id = campaign_repo::insert_step_outbound(
            &mut tx,
            mail_account_id,
            channel_id,
            sender_identity_id,
            email_address_id,
            None,
            launch_id,
            revision_id,
            enrollment_id,
            step_id,
            fixture.system_principal_id,
            "sender@example.test",
            &normalized.original,
            "Subject",
            "Body",
            Some("<p>Body</p>"),
            &[0x31; 32],
        )
        .await
        .unwrap();
        export_recipients.push(marketing_exports::NewExportRecipient {
            email_address_id,
            outbound_message_id: outbound_id,
            normalized_address: normalized.normalized_key,
            variables: serde_json::json!({}),
            sync_status: "active".into(),
        });
    }
    let export = marketing_exports::insert_export(
        &mut tx,
        marketing_exports::NewMarketingExport {
            launch_id,
            revision_id,
            step_id,
            delivery_channel_id: channel_id,
            connection_id: fixture.connection_id,
            source_manifest_hash: vec![0x41; 32],
            content_hash: vec![0x42; 32],
            audience_hash: vec![0x43; 32],
            export_hash: vec![0x44; 32],
            subject: "Exported subject".into(),
            body_text: "Exported text".into(),
            body_html: "<p>Exported HTML</p>".into(),
            sender_address: "sender@example.test".into(),
            sender_name: "Campaigns Sync Sender".into(),
            field_map: serde_json::json!({}),
            prepared_by: fixture.system_principal_id,
            expires_at,
        },
    )
    .await
    .unwrap();
    marketing_exports::replace_recipients(&mut tx, export.id, &export_recipients)
        .await
        .unwrap();

    let mut current = marketing_exports::get_export(&mut tx, export.id)
        .await
        .unwrap()
        .unwrap();
    for next in [
        MarketingExportState::Syncing,
        MarketingExportState::Ready,
        MarketingExportState::Submitting,
        MarketingExportState::InProgress,
    ] {
        if current.state == next.as_str() {
            continue;
        }
        if !next_state_reachable(final_state, next) {
            break;
        }
        assert!(
            marketing_exports::update_state(&mut tx, export.id, current.version, next)
                .await
                .unwrap()
        );
        current = marketing_exports::get_export(&mut tx, export.id)
            .await
            .unwrap()
            .unwrap();
        if next == final_state {
            break;
        }
    }
    marketing_exports::update_execution_metadata(
        &mut tx,
        export.id,
        marketing_exports::ExecutionMetadata {
            provider_list_key: Some("list-sync-1".to_string()),
            campaign_key: Some(campaign_key.to_string()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        campaign_repo::freeze_launch_row(&mut tx, launch_id, &[0x51; 32])
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        campaign_repo::transition_launch(&mut tx, launch_id, "awaiting_approval", None)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        campaign_repo::transition_launch(&mut tx, launch_id, "approved", None)
            .await
            .unwrap(),
        1
    );
    tx.commit().await.unwrap();
    export.id
}

fn next_state_reachable(final_state: MarketingExportState, next: MarketingExportState) -> bool {
    match final_state {
        MarketingExportState::Ready => matches!(
            next,
            MarketingExportState::Syncing | MarketingExportState::Ready
        ),
        MarketingExportState::InProgress => true,
        _ => false,
    }
}

async fn enqueue_poll(fixture: &CampaignsFixture, export_id: Uuid, suffix: &str) {
    let mut tx = fixture.api_db.begin(fixture.workspace_id).await.unwrap();
    jobs::enqueue(
        &mut tx,
        marketing_jobs::POLL_EXPORT_KIND,
        1,
        serde_json::json!({"export_id": export_id}),
        Utc::now(),
        Some(&format!("test_poll_export:{export_id}:{suffix}")),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn office_suppression_is_pushed_to_active_campaigns_once_and_journaled() {
    let mock = mock_campaigns_server(vec![MockReply {
        status: 200,
        body:
            r#"{"status":"success","code":"0","message":"User successfully moved to Do-Not-Mail."}"#
                .into(),
    }])
    .await;
    let fixture = fixture("push-sync@example.test").await;
    let suppression_id = add_office_optout(&fixture).await;
    enqueue_push(&fixture, fixture.email_address_id, "first").await;
    let mut registry = JobRegistry::new();
    register(&fixture, &mock.base_url, &mut registry);
    let runner = Runner::new(fixture.worker_db.clone(), registry);

    assert!(
        runner
            .run_workspace_once(fixture.workspace_id)
            .await
            .unwrap()
            >= 1
    );
    let mut api_tx = fixture.api_db.begin(fixture.workspace_id).await.unwrap();
    let operation_status: String = sqlx::query_scalar(
        "SELECT status FROM marketing_suppression_operations
          WHERE workspace_id=$1 AND suppression_id=$2 AND connection_id=$3",
    )
    .bind(fixture.workspace_id)
    .bind(suppression_id)
    .bind(fixture.connection_id)
    .fetch_one(api_tx.conn())
    .await
    .unwrap();
    api_tx.rollback().await.unwrap();
    assert_eq!(operation_status, "completed");
    let requests = mock.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path, "/api/v1.1/json/contactdonotmail");
    assert_eq!(
        requests[0].authorization.as_deref(),
        Some("Zoho-oauthtoken loopback-access-token")
    );

    enqueue_push(&fixture, fixture.email_address_id, "duplicate").await;
    assert!(
        runner
            .run_workspace_once(fixture.workspace_id)
            .await
            .unwrap()
            >= 1
    );
    assert_eq!(mock.requests.lock().unwrap().len(), 1);
    mock.task.abort();
}

#[tokio::test]
async fn suppression_sync_status_is_admin_scoped_and_reports_active_connections() {
    let fixture = fixture("sync-status@example.test").await;
    let suppression_id = add_office_optout(&fixture).await;

    let rows = application::marketing::suppressions::get_suppression_sync_status(
        &fixture.api_db,
        &fixture.admin_ctx,
        suppression_id,
    )
    .await
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].connection_id, fixture.connection_id);
    assert_eq!(rows[0].status, "pending");
    assert!(rows[0].provider_code.is_none());
    assert!(rows[0].updated_at.is_none());

    let denied = application::marketing::suppressions::get_suppression_sync_status(
        &fixture.api_db,
        &fixture.member_ctx,
        suppression_id,
    )
    .await
    .unwrap_err();
    assert_eq!(denied.code.as_str(), "FORBIDDEN");

    let foreign = seed_workspace(&fixture.api_db).await.unwrap();
    let normalized = domain::email_address::normalize(
        "foreign-suppression@example.test",
        domain::email_address::LocalPartPolicy::Lowercase,
    )
    .unwrap();
    let mut tx = fixture.api_db.begin(foreign.workspace_id).await.unwrap();
    let foreign_address: Uuid = sqlx::query_scalar(
        "INSERT INTO email_addresses
            (workspace_id, address_original, normalized_key, kind, verification_status, verified_at)
         VALUES ($1, $2, $3, 'person', 'verified', now())
         RETURNING id",
    )
    .bind(foreign.workspace_id)
    .bind(normalized.original)
    .bind(normalized.normalized_key)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let foreign_suppression = compliance_core::insert_suppression(
        &mut tx,
        foreign_address,
        "marketing",
        "unsubscribe",
        "manual",
        Some(foreign.admin.principal_id),
        None,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let hidden = application::marketing::suppressions::get_suppression_sync_status(
        &fixture.api_db,
        &fixture.admin_ctx,
        foreign_suppression,
    )
    .await
    .unwrap_err();
    assert_eq!(hidden.code.as_str(), "NOT_FOUND");

    let mut tx = fixture.api_db.begin(fixture.workspace_id).await.unwrap();
    marketing::mark_status(&mut tx, fixture.connection_id, "disabled", None)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let disconnected = application::marketing::suppressions::get_suppression_sync_status(
        &fixture.api_db,
        &fixture.admin_ctx,
        suppression_id,
    )
    .await
    .unwrap();
    assert_eq!(disconnected.len(), 1);
    assert_eq!(disconnected[0].status, "not_connected");
}

#[tokio::test]
async fn office_public_unsubscribe_runs_pause_then_journaled_zoho_push() {
    let mock = mock_campaigns_server(vec![MockReply {
        status: 200,
        body:
            r#"{"status":"success","code":"0","message":"User successfully moved to Do-Not-Mail."}"#
                .into(),
    }])
    .await;
    let fixture = fixture("public-unsubscribe-sync@example.test").await;
    let token = issue_public_unsubscribe_token(&fixture).await;
    let preview =
        application::compliance::public_unsubscribe::confirmation(&fixture.api_db, &token)
            .await
            .unwrap();
    assert_eq!(preview.status, "pending_confirmation");

    let response = application::compliance::public_unsubscribe::unsubscribe(
        &fixture.api_db,
        &token,
        "public-unsubscribe-worker-test",
    )
    .await
    .unwrap();
    assert_eq!(response.status, "unsubscribed");
    let mut tx = fixture.api_db.begin(fixture.workspace_id).await.unwrap();
    let suppression_id: Uuid = sqlx::query_scalar(
        "SELECT id FROM suppressions
          WHERE workspace_id=$1 AND email_address_id=$2
            AND purpose_scope='marketing' AND reason='unsubscribe' AND released_at IS NULL",
    )
    .bind(fixture.workspace_id)
    .bind(fixture.email_address_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let pause_jobs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs
          WHERE workspace_id=$1 AND kind='compliance.pause_enrollments'
            AND payload->>'suppression_id'=$2 AND state='queued'",
    )
    .bind(fixture.workspace_id)
    .bind(suppression_id.to_string())
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(pause_jobs, 1);
    let before_worker = application::marketing::suppressions::get_suppression_sync_status(
        &fixture.api_db,
        &fixture.admin_ctx,
        suppression_id,
    )
    .await
    .unwrap();
    assert_eq!(before_worker[0].status, "pending");

    let mut registry = JobRegistry::new();
    worker::handlers::campaigns::register(&mut registry);
    register(&fixture, &mock.base_url, &mut registry);
    let runner = Runner::new(fixture.worker_db.clone(), registry);
    assert_eq!(
        runner
            .run_workspace_once(fixture.workspace_id)
            .await
            .unwrap(),
        1
    );
    assert!(mock.requests.lock().unwrap().is_empty());
    assert_eq!(
        runner
            .run_workspace_once(fixture.workspace_id)
            .await
            .unwrap(),
        1
    );

    let after_worker = application::marketing::suppressions::get_suppression_sync_status(
        &fixture.api_db,
        &fixture.admin_ctx,
        suppression_id,
    )
    .await
    .unwrap();
    assert_eq!(after_worker[0].status, "completed");
    assert!(after_worker[0].updated_at.is_some());
    let requests = mock.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/api/v1.1/json/contactdonotmail");
    let mut tx = fixture.worker_db.begin(fixture.workspace_id).await.unwrap();
    let event: infrastructure::outbox::OutboxRow = sqlx::query_as(
        "SELECT * FROM outbox_events WHERE workspace_id=$1
         AND event_type='suppression.created' AND aggregate_id=$2",
    )
    .bind(fixture.workspace_id)
    .bind(suppression_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    application::crm::projector::handle(&mut tx, &event)
        .await
        .unwrap();
    let actor: Option<Uuid> = sqlx::query_scalar(
        "SELECT actor_principal_id FROM activities WHERE workspace_id=$1 AND source_event_id=$2",
    )
    .bind(fixture.workspace_id)
    .bind(event.id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert!(
        actor.is_none(),
        "public unsubscribe records an anonymous CRM activity"
    );
    tx.rollback().await.unwrap();
    mock.task.abort();
}

#[tokio::test]
async fn connecting_campaigns_backfills_active_office_suppressions_in_db_batches() {
    let mock = mock_campaigns_server(vec![
        MockReply {
            status: 200,
            body: r#"{"status":"success","code":"0"}"#.into(),
        },
        MockReply {
            status: 200,
            body: r#"{"status":"success","code":"0"}"#.into(),
        },
    ])
    .await;
    let fixture = fixture("backfill-first@example.test").await;
    let mut tx = fixture.api_db.begin(fixture.workspace_id).await.unwrap();
    marketing::mark_status(&mut tx, fixture.connection_id, "disabled", None)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let first_suppression = add_office_optout(&fixture).await;
    let (second_address_id, second_suppression) =
        add_suppressed_address(&fixture, "backfill-second@example.test").await;

    let mut tx = fixture.api_db.begin(fixture.workspace_id).await.unwrap();
    marketing::mark_status(&mut tx, fixture.connection_id, "active", None)
        .await
        .unwrap();
    infrastructure::repo::marketing_suppressions::enqueue_suppression_backfill_in_tx(
        &mut tx,
        fixture.connection_id,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mut registry = JobRegistry::new();
    register(&fixture, &mock.base_url, &mut registry);
    let mut runner = Runner::new(fixture.worker_db.clone(), registry);
    runner.concurrency = 1;
    assert_eq!(
        runner
            .run_workspace_once(fixture.workspace_id)
            .await
            .unwrap(),
        1
    );
    assert!(mock.requests.lock().unwrap().is_empty());
    assert_eq!(
        runner
            .run_workspace_once(fixture.workspace_id)
            .await
            .unwrap(),
        2
    );
    assert_eq!(mock.requests.lock().unwrap().len(), 2);

    let mut tx = fixture.api_db.begin(fixture.workspace_id).await.unwrap();
    let completed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM marketing_suppression_operations
          WHERE workspace_id=$1 AND connection_id=$2 AND status='completed'
            AND suppression_id = ANY($3)",
    )
    .bind(fixture.workspace_id)
    .bind(fixture.connection_id)
    .bind(vec![first_suppression, second_suppression])
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(completed, 2);
    assert_ne!(second_address_id, fixture.email_address_id);
    mock.task.abort();
}

#[tokio::test]
async fn unknown_do_not_mail_result_is_durable_and_is_never_resubmitted() {
    let mock = mock_campaigns_server(vec![MockReply {
        status: 500,
        body: r#"{"code":"500","message":"provider failed after acceptance may be unknown"}"#
            .into(),
    }])
    .await;
    let fixture = fixture("unknown-sync@example.test").await;
    let suppression_id = add_office_optout(&fixture).await;
    enqueue_push(&fixture, fixture.email_address_id, "first").await;
    let mut registry = JobRegistry::new();
    register(&fixture, &mock.base_url, &mut registry);
    let runner = Runner::new(fixture.worker_db.clone(), registry);

    runner
        .run_workspace_once(fixture.workspace_id)
        .await
        .unwrap();
    let mut api_tx = fixture.api_db.begin(fixture.workspace_id).await.unwrap();
    let operation_status: String = sqlx::query_scalar(
        "SELECT status FROM marketing_suppression_operations
          WHERE workspace_id=$1 AND suppression_id=$2 AND connection_id=$3",
    )
    .bind(fixture.workspace_id)
    .bind(suppression_id)
    .bind(fixture.connection_id)
    .fetch_one(api_tx.conn())
    .await
    .unwrap();
    api_tx.rollback().await.unwrap();
    assert_eq!(operation_status, "unknown");

    enqueue_push(&fixture, fixture.email_address_id, "later").await;
    runner
        .run_workspace_once(fixture.workspace_id)
        .await
        .unwrap();
    assert_eq!(mock.requests.lock().unwrap().len(), 1);
    mock.task.abort();
}

#[tokio::test]
async fn stale_started_operation_becomes_unknown_without_resubmitting_to_zoho() {
    let mock = mock_campaigns_server(vec![MockReply {
        status: 200,
        body: r#"{"status":"success","code":"0"}"#.into(),
    }])
    .await;
    let fixture = fixture("stale-sync@example.test").await;
    let suppression_id = add_office_optout(&fixture).await;
    let mut tx = fixture.worker_db.begin(fixture.workspace_id).await.unwrap();
    sqlx::query(
        "INSERT INTO marketing_suppression_operations
            (workspace_id, suppression_id, email_address_id, connection_id, status,
             created_at, updated_at)
         VALUES ($1, $2, $3, $4, 'started', now() - interval '3 minutes',
                 now() - interval '3 minutes')",
    )
    .bind(fixture.workspace_id)
    .bind(suppression_id)
    .bind(fixture.email_address_id)
    .bind(fixture.connection_id)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let visible_started = application::marketing::suppressions::get_suppression_sync_status(
        &fixture.api_db,
        &fixture.admin_ctx,
        suppression_id,
    )
    .await
    .unwrap();
    assert_eq!(visible_started[0].status, "in_progress");
    enqueue_push(&fixture, fixture.email_address_id, "stale").await;
    let mut registry = JobRegistry::new();
    register(&fixture, &mock.base_url, &mut registry);
    let runner = Runner::new(fixture.worker_db.clone(), registry);
    assert_eq!(
        runner
            .run_workspace_once(fixture.workspace_id)
            .await
            .unwrap(),
        1
    );
    assert!(mock.requests.lock().unwrap().is_empty());

    let mut tx = fixture.api_db.begin(fixture.workspace_id).await.unwrap();
    let status: (String, Option<String>) = sqlx::query_as(
        "SELECT status, error_code FROM marketing_suppression_operations
          WHERE workspace_id=$1 AND suppression_id=$2 AND connection_id=$3",
    )
    .bind(fixture.workspace_id)
    .bind(suppression_id)
    .bind(fixture.connection_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(status.0, "unknown");
    assert_eq!(status.1.as_deref(), Some("stale_provider_outcome"));
    let visible_status = application::marketing::suppressions::get_suppression_sync_status(
        &fixture.api_db,
        &fixture.admin_ctx,
        suppression_id,
    )
    .await
    .unwrap();
    assert_eq!(visible_status[0].status, "unknown");
    assert_eq!(
        visible_status[0].provider_code.as_deref(),
        Some("stale_provider_outcome")
    );
    let mut tx = fixture.api_db.begin(fixture.workspace_id).await.unwrap();
    marketing::mark_status(&mut tx, fixture.connection_id, "disabled", None)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let historical_status = application::marketing::suppressions::get_suppression_sync_status(
        &fixture.api_db,
        &fixture.admin_ctx,
        suppression_id,
    )
    .await
    .unwrap();
    assert_eq!(historical_status.len(), 1);
    assert_eq!(historical_status[0].connection_id, fixture.connection_id);
    assert_eq!(historical_status[0].status, "unknown");
    mock.task.abort();
}

#[tokio::test]
async fn poll_imports_optouts_and_hard_bounces_as_office_suppressions_with_partial_report() {
    let mock = mock_campaigns_server(vec![
        MockReply {
            status: 200,
            body: r#"{"status":"success","code":"0","requestdetails":{"range":100,"fromindex":1},"list_of_details":[]}"#.into(),
        },
        MockReply {
            status: 200,
            body: r#"{"status":"success","code":"0","campaign_status":"inprogress","campaign-details":[{"campaign_key":"campaign-sync-1"}]}"#.into(),
        },
        MockReply {
            status: 200,
            body: r#"{"status":"success","code":"0","campaign-reports":[{"emails_sent_count":"1","opens_count":"1"}]}"#.into(),
        },
        MockReply {
            status: 200,
            body: r#"{"status":"success","code":"0","requestdetails":{"range":20,"fromindex":1},"list_of_details":[{"contactemailaddress":"poll-sync@example.test","contactid":"contact-1"}]}"#.into(),
        },
        MockReply {
            status: 200,
            body: r#"{"status":"success","code":"0","requestdetails":{"range":20,"fromindex":1},"list_of_details":[{"contactemailaddress":"poll-sync@example.test","contactid":"contact-1"}]}"#.into(),
        },
    ])
    .await;
    let fixture = fixture("poll-sync@example.test").await;
    let export_id = add_export_snapshot(
        &fixture,
        "campaign-sync-1",
        MarketingExportState::InProgress,
        0,
        false,
    )
    .await;
    enqueue_poll(&fixture, export_id, "first").await;
    let mut registry = JobRegistry::new();
    register(&fixture, &mock.base_url, &mut registry);
    let runner = Runner::new(fixture.worker_db.clone(), registry);

    assert_eq!(
        runner
            .run_workspace_once(fixture.workspace_id)
            .await
            .unwrap(),
        1
    );
    let requests = mock.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 5);
    assert!(
        requests[0]
            .path
            .starts_with("/api/v1.1/getlistsubscribers?")
    );
    assert!(
        requests[1]
            .path
            .starts_with("/api/v1.1/getcampaigndetails?")
    );
    assert!(requests[2].path.starts_with("/api/v1.1/campaignreports?"));
    assert_eq!(requests[3].path, "/api/v1.1/getcampaignrecipientsdata");
    assert_eq!(requests[4].path, "/api/v1.1/getcampaignrecipientsdata");
    assert_eq!(requests[3].method, "POST");
    assert_eq!(requests[4].method, "POST");
    assert!(requests.iter().all(|request| {
        request.authorization.as_deref() == Some("Zoho-oauthtoken loopback-access-token")
    }));
    assert!(requests[3].body.contains("action=optoutcontacts"));
    assert!(requests[4].body.contains("action=senthardbounce"));

    let mut tx = fixture.api_db.begin(fixture.workspace_id).await.unwrap();
    let export = marketing_exports::get_export(&mut tx, export_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(export.state, "in_progress");
    let launch_status: String =
        sqlx::query_scalar("SELECT status FROM campaign_launches WHERE workspace_id=$1 AND id=$2")
            .bind(fixture.workspace_id)
            .bind(export.launch_id)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    assert_eq!(launch_status, "running");
    assert_eq!(export.report["recipient_reports"]["coverage"], "partial");
    assert_eq!(
        export.report["recipient_reports"]["campaign_optouts"]["partial"],
        true
    );
    assert_eq!(
        export.report["recipient_reports"]["hard_bounces"]["partial"],
        true
    );
    assert_eq!(
        export.report["aggregate"]["metrics"]["emails_sent_count"],
        "1"
    );
    assert_eq!(
        marketing_exports::list_recipients(&mut tx, export_id)
            .await
            .unwrap()[0]
            .sync_status,
        "bounced"
    );
    let reasons: Vec<String> = sqlx::query_scalar(
        "SELECT reason FROM suppressions
          WHERE workspace_id = $1 AND email_address_id = $2 AND released_at IS NULL
          ORDER BY reason",
    )
    .bind(fixture.workspace_id)
    .bind(fixture.email_address_id)
    .fetch_all(tx.conn())
    .await
    .unwrap();
    assert_eq!(reasons, vec!["hard_bounce", "unsubscribe"]);
    let imported_sources: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM suppression_events e
          JOIN suppressions s ON s.workspace_id = e.workspace_id AND s.id = e.suppression_id
          WHERE e.workspace_id = $1 AND s.email_address_id = $2
            AND e.source_kind = 'zoho_campaigns_poll'",
    )
    .bind(fixture.workspace_id)
    .bind(fixture.email_address_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(imported_sources, 2);
    tx.rollback().await.unwrap();
    mock.task.abort();
}

#[tokio::test]
async fn poll_follows_multiple_pages_and_imports_more_than_twenty_list_unsubscribers() {
    let fixture = fixture("unsubscribe-page@example.test").await;
    let mut first_page = vec![serde_json::json!({
        "contact_email": fixture.normalized_email,
        "contactstatus": "unsub"
    })];
    first_page.extend((0..20).map(|index| {
        serde_json::json!({
            "contact_email": format!("pagination-{index}@example.test"),
            "contactstatus": "unsub"
        })
    }));
    first_page.extend((0..79).map(|index| {
        serde_json::json!({
            "contact_email": format!("unrelated-{index}@example.test"),
            "contactstatus": "unsub"
        })
    }));
    let page = |from_index, contacts| MockReply {
        status: 200,
        body: serde_json::json!({
            "status": "success",
            "code": "0",
            "requestdetails": {"fromindex": from_index, "range": 100, "status": "unsub"},
            "list_of_details": contacts
        })
        .to_string(),
    };
    let empty_recipient_action = MockReply {
        status: 200,
        body: r#"{"status":"success","code":"0","requestdetails":{"range":20,"fromindex":1},"list_of_details":[]}"#.into(),
    };
    let mock = mock_campaigns_server(vec![
        page(1, first_page),
        page(
            101,
            vec![serde_json::json!({
                "contact_email": "pagination-20@example.test",
                "contactstatus": "unsub"
            })],
        ),
        MockReply {
            status: 200,
            body: r#"{"status":"success","code":"0","campaign_status":"inprogress","campaign-details":[{"campaign_key":"campaign-pagination-1"}]}"#.into(),
        },
        MockReply {
            status: 200,
            body: r#"{"status":"success","code":"0","campaign-reports":[{"emails_sent_count":"22"}]}"#.into(),
        },
        empty_recipient_action.clone(),
        empty_recipient_action,
    ])
    .await;
    let export_id = add_export_snapshot(
        &fixture,
        "campaign-pagination-1",
        MarketingExportState::InProgress,
        21,
        false,
    )
    .await;
    enqueue_poll(&fixture, export_id, "first").await;
    let mut registry = JobRegistry::new();
    register(&fixture, &mock.base_url, &mut registry);
    let runner = Runner::new(fixture.worker_db.clone(), registry);

    assert_eq!(
        runner
            .run_workspace_once(fixture.workspace_id)
            .await
            .unwrap(),
        1
    );
    let requests = mock.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 6);
    assert!(requests[0].path.contains("fromindex=1"));
    assert!(requests[0].path.contains("status=unsub"));
    assert!(requests[1].path.contains("fromindex=101"));

    let mut tx = fixture.api_db.begin(fixture.workspace_id).await.unwrap();
    let export = marketing_exports::get_export(&mut tx, export_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        export.report["recipient_reports"]["list_unsubscribers"]["count"],
        22
    );
    assert_eq!(
        export.report["recipient_reports"]["list_unsubscribers"]["coverage"],
        "complete"
    );
    assert_eq!(
        export.report["recipient_reports"]["list_unsubscribers"]["partial"],
        false
    );
    let recipients = marketing_exports::list_recipients(&mut tx, export_id)
        .await
        .unwrap();
    assert_eq!(recipients.len(), 22);
    assert!(
        recipients
            .iter()
            .all(|recipient| recipient.sync_status == "unsubscribed")
    );
    let reason_counts: (i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE reason = 'unsubscribe'),
                count(*) FILTER (WHERE reason = 'hard_bounce')
           FROM suppressions
          WHERE workspace_id = $1 AND email_address_id IN (
                SELECT email_address_id FROM marketing_export_recipients
                 WHERE workspace_id = $1 AND export_id = $2)
            AND released_at IS NULL",
    )
    .bind(fixture.workspace_id)
    .bind(export_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(reason_counts, (22, 0));
    tx.rollback().await.unwrap();
    mock.task.abort();
}

#[tokio::test]
async fn completed_export_continues_slow_unsubscribe_polling_without_campaign_mutations() {
    let fixture = fixture("completed-poll@example.test").await;
    let empty_list = || {
        MockReply {
        status: 200,
        body: r#"{"status":"success","code":"0","requestdetails":{"range":100,"fromindex":1},"list_of_details":[]}"#.into(),
    }
    };
    let list_unsubscribe = || MockReply {
        status: 200,
        body: serde_json::json!({
            "status": "success",
            "code": "0",
            "requestdetails": {"range": 100, "fromindex": 1, "status": "unsub"},
            "list_of_details": [{
                "contact_email": fixture.normalized_email.clone(),
                "contactstatus": "unsub"
            }]
        })
        .to_string(),
    };
    let completed_details = || {
        MockReply {
        status: 200,
        body: r#"{"status":"success","code":"0","campaign_status":"Sent","campaign-details":[{"campaign_key":"campaign-completed-poll"}]}"#.into(),
    }
    };
    let report = || MockReply {
        status: 200,
        body: r#"{"status":"success","code":"0","campaign-reports":[{"emails_sent_count":"1"}]}"#
            .into(),
    };
    let empty_recipient_action = || {
        MockReply {
        status: 200,
        body: r#"{"status":"success","code":"0","requestdetails":{"range":20,"fromindex":1},"list_of_details":[]}"#.into(),
    }
    };
    let mock = mock_campaigns_server(vec![
        empty_list(),
        completed_details(),
        report(),
        empty_recipient_action(),
        empty_recipient_action(),
        list_unsubscribe(),
        completed_details(),
        report(),
        empty_recipient_action(),
        empty_recipient_action(),
    ])
    .await;
    let export_id = add_export_snapshot(
        &fixture,
        "campaign-completed-poll",
        MarketingExportState::InProgress,
        0,
        true,
    )
    .await;
    let mut api_tx = fixture.api_db.begin(fixture.workspace_id).await.unwrap();
    let launch_id = marketing_exports::get_export(&mut api_tx, export_id)
        .await
        .unwrap()
        .unwrap()
        .launch_id;
    api_tx.commit().await.unwrap();
    enqueue_poll(&fixture, export_id, "initial").await;
    let mut registry = JobRegistry::new();
    register(&fixture, &mock.base_url, &mut registry);
    let runner = Runner::new(fixture.worker_db.clone(), registry);

    assert_eq!(
        runner
            .run_workspace_once(fixture.workspace_id)
            .await
            .unwrap(),
        1
    );
    let mut api_tx = fixture.api_db.begin(fixture.workspace_id).await.unwrap();
    let first_export = marketing_exports::get_export(&mut api_tx, export_id)
        .await
        .unwrap()
        .unwrap();
    let launch_status: String =
        sqlx::query_scalar("SELECT status FROM campaign_launches WHERE workspace_id=$1 AND id=$2")
            .bind(fixture.workspace_id)
            .bind(launch_id)
            .fetch_one(api_tx.conn())
            .await
            .unwrap();
    let future_poll_at: Option<chrono::DateTime<Utc>> = sqlx::query_scalar(
        "SELECT min(available_at) FROM jobs
          WHERE workspace_id=$1 AND kind='marketing.poll_export'
            AND payload->>'export_id'=$2 AND available_at > now()",
    )
    .bind(fixture.workspace_id)
    .bind(export_id.to_string())
    .fetch_one(api_tx.conn())
    .await
    .unwrap();
    api_tx.rollback().await.unwrap();
    assert_eq!(first_export.state, "completed");
    assert_eq!(launch_status, "completed");
    let future_poll_at = future_poll_at.expect("completed export should keep a future poll");
    assert!(
        (future_poll_at - Utc::now()).num_minutes() >= 55,
        "completed exports should use the slower hourly polling cadence"
    );

    enqueue_poll(&fixture, export_id, "later").await;
    assert_eq!(
        runner
            .run_workspace_once(fixture.workspace_id)
            .await
            .unwrap(),
        1
    );
    let requests = mock.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 10);
    assert!(
        requests[5]
            .path
            .starts_with("/api/v1.1/getlistsubscribers?")
    );
    assert!(requests[5].path.contains("status=unsub"));
    assert!(requests.iter().all(|request| {
        !request.path.contains("sendcampaign")
            && !request.path.contains("createCampaign")
            && !request.path.contains("addlistandcontacts")
            && !request.path.ends_with("/subscribe")
    }));

    let mut api_tx = fixture.api_db.begin(fixture.workspace_id).await.unwrap();
    let export = marketing_exports::get_export(&mut api_tx, export_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(export.state, "completed");
    let reason: String = sqlx::query_scalar(
        "SELECT reason FROM suppressions
          WHERE workspace_id=$1 AND email_address_id=$2 AND released_at IS NULL",
    )
    .bind(fixture.workspace_id)
    .bind(fixture.email_address_id)
    .fetch_one(api_tx.conn())
    .await
    .unwrap();
    assert_eq!(reason, "unsubscribe");
    assert_eq!(
        marketing_exports::list_recipients(&mut api_tx, export_id)
            .await
            .unwrap()[0]
            .sync_status,
        "unsubscribed"
    );
    let outbound_status: String = sqlx::query_scalar(
        "SELECT o.status FROM outbound_messages o
          JOIN marketing_export_recipients r
            ON r.workspace_id=o.workspace_id AND r.outbound_message_id=o.id
         WHERE r.workspace_id=$1 AND r.export_id=$2",
    )
    .bind(fixture.workspace_id)
    .bind(export_id)
    .fetch_one(api_tx.conn())
    .await
    .unwrap();
    assert_eq!(outbound_status, "draft");
    assert_eq!(
        export.report["recipient_reports"]["list_unsubscribers"]["count"],
        1
    );
    api_tx.rollback().await.unwrap();
    mock.task.abort();
}

#[tokio::test]
async fn ready_export_with_office_suppression_is_canceled_without_campaigns_io() {
    let mock = mock_campaigns_server(vec![]).await;
    let fixture = fixture("ready-suppressed@example.test").await;
    let export_id = add_export_snapshot(
        &fixture,
        "campaign-ready-1",
        MarketingExportState::Ready,
        0,
        false,
    )
    .await;
    add_office_optout(&fixture).await;
    enqueue_poll(&fixture, export_id, "first").await;
    let mut registry = JobRegistry::new();
    register(&fixture, &mock.base_url, &mut registry);
    let runner = Runner::new(fixture.worker_db.clone(), registry);

    assert_eq!(
        runner
            .run_workspace_once(fixture.workspace_id)
            .await
            .unwrap(),
        1
    );
    assert!(mock.requests.lock().unwrap().is_empty());
    let mut tx = fixture.api_db.begin(fixture.workspace_id).await.unwrap();
    assert_eq!(
        marketing_exports::get_export(&mut tx, export_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        "canceled"
    );
    assert!(
        marketing_exports::get_export(&mut tx, export_id)
            .await
            .unwrap()
            .unwrap()
            .last_checked_at
            .is_some()
    );
    assert_eq!(
        marketing_exports::list_recipients(&mut tx, export_id)
            .await
            .unwrap()[0]
            .sync_status,
        "excluded"
    );
    tx.rollback().await.unwrap();
    mock.task.abort();
}

#[tokio::test]
async fn expired_ready_export_is_canceled_and_poll_chain_stops_without_campaigns_io() {
    let mock = mock_campaigns_server(vec![]).await;
    let fixture = fixture("expired-ready@example.test").await;
    let export_id = add_export_snapshot(
        &fixture,
        "campaign-expired-ready",
        MarketingExportState::Ready,
        0,
        true,
    )
    .await;
    enqueue_poll(&fixture, export_id, "expired").await;
    let mut registry = JobRegistry::new();
    register(&fixture, &mock.base_url, &mut registry);
    let runner = Runner::new(fixture.worker_db.clone(), registry);

    assert_eq!(
        runner
            .run_workspace_once(fixture.workspace_id)
            .await
            .unwrap(),
        1
    );
    assert!(mock.requests.lock().unwrap().is_empty());
    let mut tx = fixture.api_db.begin(fixture.workspace_id).await.unwrap();
    let export = marketing_exports::get_export(&mut tx, export_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(export.state, "canceled");
    assert_eq!(
        export.report["office_suppression_check"],
        "expired_ready_export"
    );
    assert!(export.last_checked_at.is_some());
    let scheduled_polls: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs
          WHERE workspace_id=$1 AND kind='marketing.poll_export'
            AND payload->>'export_id'=$2 AND available_at > now()",
    )
    .bind(fixture.workspace_id)
    .bind(export_id.to_string())
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(scheduled_polls, 0);
    tx.rollback().await.unwrap();
    mock.task.abort();
}
