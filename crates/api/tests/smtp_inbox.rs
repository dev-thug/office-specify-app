//! Admin-controlled SMTP-profile IMAP inbox sync.
//!
//! These integration tests require `SMTP_REPLY_TEST_DATABASE_URL` to point at
//! the dedicated `outreach_smtp_reply_verify` database. IMAP reads use a fake
//! source; no external mailbox connection is made.

use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use api::{AppState, testing::TestApp};
use async_trait::async_trait;
use chrono::Utc;
use infrastructure::{
    config::Config,
    db::{Db, ROLE_API, ROLE_WORKER},
    providers::{
        ProviderError, ProviderMessage, SecretValue,
        imap_reply::{ImapCursor, ImapInboxPage, ImapInboxSource, imap_provider_message_id},
    },
    testing::{TEST_PASSWORD, TestPrincipal, TestWorld, seed_workspace},
};
use reqwest::{Response, StatusCode};
use serde_json::{Value, json};
use tempfile::TempDir;
use uuid::Uuid;
use worker::{JobRegistry, Runner};

const IMAP_HOST: &str = "imappro.zoho.com";
const SMTP_PASSWORD: &str = "smtp-reuse-test-secret";

struct SmtpInboxTest {
    app: TestApp,
    api_db: Db,
    worker_db: Db,
    world: TestWorld,
    _secrets: TempDir,
}

async fn setup() -> Option<SmtpInboxTest> {
    let url = match std::env::var("SMTP_REPLY_TEST_DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("SMTP_REPLY_TEST_DATABASE_URL not set — skipping SMTP inbox test");
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
        database, "outreach_smtp_reply_verify",
        "SMTP inbox tests must use the dedicated outreach_smtp_reply_verify database"
    );

    let owner = Db::connect(&url, None).await.unwrap();
    owner.migrate().await.unwrap();
    let api_db = Db::connect(&url, Some(ROLE_API)).await.unwrap();
    let worker_db = Db::connect(&url, Some(ROLE_WORKER)).await.unwrap();
    let world = seed_workspace(&api_db).await.unwrap();
    let secrets = tempfile::tempdir().unwrap();
    let mut config = Config::test();
    config.secrets_file = secrets.path().join("smtp-inbox-secrets.json");
    let app = TestApp::spawn_with_state(AppState::with_fake_providers(
        api_db
            .clone()
            .with_default_workspace(Some(world.workspace_id)),
        Arc::new(config),
        None,
    ))
    .await;
    Some(SmtpInboxTest {
        app,
        api_db,
        worker_db,
        world,
        _secrets: secrets,
    })
}

async fn login(app: &TestApp, principal: &TestPrincipal) -> String {
    let response = app
        .login(principal.email.as_deref().unwrap(), TEST_PASSWORD)
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    TestApp::csrf_token_from(&response).expect("CSRF cookie")
}

fn profile_body(mail_account_id: Uuid, name: &str, password: &str) -> Value {
    json!({
        "name": name,
        "mail_account_id": mail_account_id,
        "host": "smtp.example.test",
        "port": 587,
        "security": "starttls",
        "username": "smtp-user",
        "password": password,
        "from_address": format!("{name}@example.test"),
        "from_name": name,
        "reply_to": null,
        "custom_headers": {},
        "sender_confirmed": true
    })
}

async fn create_profile(
    app: &TestApp,
    csrf: &str,
    mail_account_id: Uuid,
    name: &str,
    password: &str,
) -> Value {
    let response = app
        .client
        .post(app.url("/api/v1/smtp-connections"))
        .header("x-csrf-token", csrf)
        .header("Idempotency-Key", format!("smtp-create-{}", Uuid::new_v4()))
        .json(&profile_body(mail_account_id, name, password))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = response.json::<Value>().await.unwrap();
    assert!(!body.to_string().contains(password));
    body["data"].clone()
}

async fn get_inbox(app: &TestApp, id: Uuid) -> Response {
    app.client
        .get(app.url(&format!("/api/v1/smtp-connections/{id}/inbox")))
        .send()
        .await
        .unwrap()
}

async fn put_inbox(
    app: &TestApp,
    csrf: &str,
    id: Uuid,
    enabled: bool,
    host: &str,
    key: &str,
) -> Response {
    app.client
        .put(app.url(&format!("/api/v1/smtp-connections/{id}/inbox")))
        .header("x-csrf-token", csrf)
        .header("Idempotency-Key", key)
        .json(&json!({"enabled": enabled, "imap_host": host}))
        .send()
        .await
        .unwrap()
}

async fn request_sync(app: &TestApp, csrf: &str, id: Uuid, key: &str) -> Response {
    app.client
        .post(app.url(&format!("/api/v1/smtp-connections/{id}/inbox/sync")))
        .header("x-csrf-token", csrf)
        .header("Idempotency-Key", key)
        .send()
        .await
        .unwrap()
}

#[derive(Debug, Clone)]
struct SourceCall {
    host: String,
    username: String,
    mail_account_id: Uuid,
    cursor: ImapCursor,
    password_matched: bool,
}

struct FakeInboxSource {
    expected_password: String,
    pages: Mutex<VecDeque<ImapInboxPage>>,
    calls: Mutex<Vec<SourceCall>>,
    password_seen: AtomicBool,
}

#[async_trait]
impl ImapInboxSource for FakeInboxSource {
    async fn fetch_inbox_page(
        &self,
        host: &str,
        username: &str,
        password: &SecretValue,
        mail_account_id: Uuid,
        cursor: ImapCursor,
    ) -> Result<ImapInboxPage, ProviderError> {
        let password_matched = password.expose() == self.expected_password;
        if password_matched {
            self.password_seen.store(true, Ordering::SeqCst);
        }
        self.calls.lock().unwrap().push(SourceCall {
            host: host.to_owned(),
            username: username.to_owned(),
            mail_account_id,
            cursor,
            password_matched,
        });
        if host != IMAP_HOST || username != "smtp-user" || !password_matched {
            return Err(ProviderError::NotConfigured);
        }
        self.pages
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| ProviderError::unavailable("fake inbox page missing"))
    }
}

fn source_message(
    mail_account_id: Uuid,
    uid_validity: u32,
    uid: u32,
    from: &str,
    internet_message_id: &str,
) -> ProviderMessage {
    ProviderMessage {
        provider_message_id: imap_provider_message_id(
            mail_account_id,
            IMAP_HOST,
            uid_validity,
            uid,
        ),
        provider_thread_id: None,
        internet_message_id: Some(internet_message_id.to_owned()),
        in_reply_to: None,
        references: Vec::new(),
        from: from.to_owned(),
        to: vec!["office@example.test".into()],
        subject: Some("Inbox response".into()),
        body_text: Some("Received from the complete inbox sync.".into()),
        body_html: None,
        message_at: Utc::now(),
    }
}

fn source_page(
    uid_validity: u32,
    last_uid: u32,
    has_more: bool,
    messages: Vec<ProviderMessage>,
) -> ImapInboxPage {
    ImapInboxPage {
        uid_validity,
        last_uid,
        has_more,
        scanned: messages.len(),
        skipped: 0,
        truncated: 0,
        messages,
    }
}

fn runner(test: &SmtpInboxTest, source: Arc<FakeInboxSource>) -> Runner {
    let mut registry = JobRegistry::new();
    worker::handlers::smtp_inbox::register_with_source(
        &mut registry,
        Arc::new(test.app.state.config.as_ref().clone()),
        source,
    );
    Runner::new(test.worker_db.clone(), registry)
}

async fn inbox_counts(
    db: &Db,
    workspace_id: Uuid,
    mail_account_id: Uuid,
    internet_message_id: &str,
) -> (i64, i64) {
    let mut tx = db.begin(workspace_id).await.unwrap();
    let total = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM email_messages
          WHERE workspace_id = $1 AND mail_account_id = $2 AND direction = 'inbound'",
    )
    .bind(workspace_id)
    .bind(mail_account_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let same_internet_id = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM email_messages
          WHERE workspace_id = $1 AND mail_account_id = $2 AND direction = 'inbound'
            AND internet_message_id = $3",
    )
    .bind(workspace_id)
    .bind(mail_account_id)
    .bind(internet_message_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    (total, same_internet_id)
}

async fn truncated_inbox_audits(db: &Db, workspace_id: Uuid) -> i64 {
    let mut tx = db.begin(workspace_id).await.unwrap();
    let count = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM audit_logs
          WHERE workspace_id = $1 AND action = 'smtp_inbox.synced'
            AND details ->> 'truncated' = '1'",
    )
    .bind(workspace_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    count
}

#[tokio::test]
async fn smtp_inbox_sync_reuses_profile_secret_imports_pages_and_restarts_on_uidvalidity_change() {
    let Some(test) = setup().await else { return };
    let csrf = login(&test.app, &test.world.admin).await;
    let profile = create_profile(
        &test.app,
        &csrf,
        test.world.mail_account_id,
        &format!("inbox-{}", Uuid::new_v4().simple()),
        SMTP_PASSWORD,
    )
    .await;
    let smtp_connection_id = Uuid::parse_str(profile["id"].as_str().unwrap()).unwrap();

    let initial = get_inbox(&test.app, smtp_connection_id).await;
    assert_eq!(initial.status(), StatusCode::OK);
    let initial = initial.json::<Value>().await.unwrap();
    assert_eq!(initial["data"]["enabled"], false);
    assert_eq!(initial["data"]["imap_host"], "");

    let enabled = put_inbox(
        &test.app,
        &csrf,
        smtp_connection_id,
        true,
        IMAP_HOST,
        "inbox-enable-key",
    )
    .await;
    assert_eq!(enabled.status(), StatusCode::OK);
    let enabled = enabled.json::<Value>().await.unwrap();
    assert_eq!(enabled["data"]["enabled"], true);
    assert_eq!(enabled["data"]["imap_host"], IMAP_HOST);
    assert_eq!(enabled["data"]["imap_port"], 993);
    assert_eq!(enabled["data"]["folder"], "INBOX");
    assert!(enabled["data"]["active_job_id"].is_string());
    assert!(!enabled.to_string().contains(SMTP_PASSWORD));
    assert!(enabled["data"].get("secret_ref").is_none());

    let enable_replay = put_inbox(
        &test.app,
        &csrf,
        smtp_connection_id,
        true,
        IMAP_HOST,
        "inbox-enable-key",
    )
    .await;
    assert_eq!(enable_replay.status(), StatusCode::OK);
    let enable_replay = enable_replay.json::<Value>().await.unwrap();
    assert_eq!(
        enable_replay["data"]["active_job_id"],
        enabled["data"]["active_job_id"]
    );

    let first = source_page(
        42,
        2,
        true,
        vec![
            source_message(
                test.world.mail_account_id,
                42,
                1,
                "billing@vendor.example",
                "<shared-id@vendor.example>",
            ),
            source_message(
                test.world.mail_account_id,
                42,
                2,
                "ops@other-vendor.example",
                "<ops-1@other-vendor.example>",
            ),
        ],
    );
    let mut second = source_page(
        42,
        5,
        false,
        vec![source_message(
            test.world.mail_account_id,
            42,
            5,
            "unknown@invalid",
            "<large-1@customer.example>",
        )],
    );
    second.truncated = 1;
    second.messages[0].body_text = Some(
        "[Office imported this inbox item with its first 524288 message bytes. The message content was truncated.]"
            .into(),
    );
    let after_uidvalidity_reset = source_page(
        99,
        1,
        false,
        vec![source_message(
            test.world.mail_account_id,
            99,
            1,
            "billing@vendor.example",
            "<shared-id@vendor.example>",
        )],
    );
    let fake = Arc::new(FakeInboxSource {
        expected_password: SMTP_PASSWORD.into(),
        pages: Mutex::new(VecDeque::from([first, second, after_uidvalidity_reset])),
        calls: Mutex::new(Vec::new()),
        password_seen: AtomicBool::new(false),
    });
    let runner = runner(&test, fake.clone());

    let manual = request_sync(&test.app, &csrf, smtp_connection_id, "inbox-sync-first").await;
    assert_eq!(manual.status(), StatusCode::ACCEPTED);
    let manual = manual.json::<Value>().await.unwrap();
    assert_eq!(manual["data"]["job_id"], enabled["data"]["active_job_id"]);
    let replay = request_sync(&test.app, &csrf, smtp_connection_id, "inbox-sync-first").await;
    assert_eq!(replay.status(), StatusCode::ACCEPTED);
    assert_eq!(
        replay.json::<Value>().await.unwrap()["data"]["job_id"],
        manual["data"]["job_id"]
    );

    assert_eq!(
        runner
            .run_workspace_once(test.world.workspace_id)
            .await
            .unwrap(),
        1
    );
    let after_first_page = get_inbox(&test.app, smtp_connection_id)
        .await
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(after_first_page["data"]["cursor_uidvalidity"], 42);
    assert_eq!(after_first_page["data"]["cursor_uid"], 2);
    assert_eq!(after_first_page["data"]["imported_messages"], 2);
    assert!(after_first_page["data"]["active_job_id"].is_string());

    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(
        runner
            .run_workspace_once(test.world.workspace_id)
            .await
            .unwrap(),
        1
    );
    let caught_up = get_inbox(&test.app, smtp_connection_id)
        .await
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(caught_up["data"]["cursor_uidvalidity"], 42);
    assert_eq!(caught_up["data"]["cursor_uid"], 5);
    assert_eq!(caught_up["data"]["imported_messages"], 3);
    assert!(caught_up["data"]["active_job_id"].is_null());
    assert!(caught_up["data"]["next_sync_at"].is_string());
    assert_eq!(
        truncated_inbox_audits(&test.api_db, test.world.workspace_id).await,
        1,
        "a truncated message remains an imported inbox item and is counted in audit details"
    );

    let reset_job = request_sync(&test.app, &csrf, smtp_connection_id, "inbox-sync-reset").await;
    assert_eq!(reset_job.status(), StatusCode::ACCEPTED);
    assert_eq!(
        runner
            .run_workspace_once(test.world.workspace_id)
            .await
            .unwrap(),
        1
    );
    let reset = get_inbox(&test.app, smtp_connection_id)
        .await
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(reset["data"]["cursor_uidvalidity"], 99);
    assert_eq!(reset["data"]["cursor_uid"], 1);
    assert_eq!(reset["data"]["imported_messages"], 4);
    assert_eq!(
        inbox_counts(
            &test.api_db,
            test.world.workspace_id,
            test.world.mail_account_id,
            "<shared-id@vendor.example>"
        )
        .await,
        (4, 2),
        "a UIDVALIDITY reset can reimport the same Internet Message-ID under a new UID"
    );

    let calls = fake.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].host, IMAP_HOST);
    assert_eq!(calls[0].username, "smtp-user");
    assert_eq!(calls[0].mail_account_id, test.world.mail_account_id);
    assert_eq!(calls[0].cursor.uid_validity, None);
    assert_eq!(calls[0].cursor.last_uid, 0);
    assert_eq!(calls[1].cursor.uid_validity, Some(42));
    assert_eq!(calls[1].cursor.last_uid, 2);
    assert_eq!(calls[2].cursor.uid_validity, Some(42));
    assert_eq!(calls[2].cursor.last_uid, 5);
    assert!(calls.iter().all(|call| call.password_matched));
    assert!(fake.password_seen.load(Ordering::SeqCst));

    let disabled = put_inbox(
        &test.app,
        &csrf,
        smtp_connection_id,
        false,
        IMAP_HOST,
        "inbox-disable-key",
    )
    .await;
    assert_eq!(disabled.status(), StatusCode::OK);
    let disabled = disabled.json::<Value>().await.unwrap();
    assert_eq!(disabled["data"]["enabled"], false);
    assert!(disabled["data"]["active_job_id"].is_null());
    assert!(disabled["data"]["next_sync_at"].is_null());
    assert!(!disabled.to_string().contains(SMTP_PASSWORD));
    assert_eq!(
        runner
            .run_workspace_once(test.world.workspace_id)
            .await
            .unwrap(),
        0,
        "disable cancels the queued periodic poll"
    );
}

#[tokio::test]
async fn smtp_inbox_admin_gate_and_foreign_profile_are_workspace_scoped() {
    let Some(test) = setup().await else { return };
    let second_world = seed_workspace(&test.api_db).await.unwrap();
    let foreign_app = TestApp::spawn_with_state(AppState::with_fake_providers(
        test.api_db
            .clone()
            .with_default_workspace(Some(second_world.workspace_id)),
        test.app.state.config.clone(),
        None,
    ))
    .await;
    let csrf_second = login(&foreign_app, &second_world.admin).await;
    let foreign_profile = create_profile(
        &foreign_app,
        &csrf_second,
        second_world.mail_account_id,
        &format!("foreign-{}", Uuid::new_v4().simple()),
        "second-workspace-secret",
    )
    .await;
    let foreign_id = Uuid::parse_str(foreign_profile["id"].as_str().unwrap()).unwrap();

    let csrf_admin = login(&test.app, &test.world.admin).await;
    let own_profile = create_profile(
        &test.app,
        &csrf_admin,
        test.world.mail_account_id,
        &format!("own-{}", Uuid::new_v4().simple()),
        "own-workspace-secret",
    )
    .await;
    let own_id = Uuid::parse_str(own_profile["id"].as_str().unwrap()).unwrap();

    let _csrf_manager = login(&test.app, &test.world.manager).await;
    let manager = get_inbox(&test.app, own_id).await;
    assert_eq!(manager.status(), StatusCode::FORBIDDEN);
    let _csrf_admin = login(&test.app, &test.world.admin).await;
    let foreign = get_inbox(&test.app, foreign_id).await;
    assert_eq!(foreign.status(), StatusCode::NOT_FOUND);
    let own = get_inbox(&test.app, own_id).await;
    assert_eq!(own.status(), StatusCode::OK);
    let own = own.json::<Value>().await.unwrap();
    assert_eq!(own["data"]["enabled"], false);
    assert!(!own.to_string().contains("own-workspace-secret"));
}

#[tokio::test]
async fn disabling_before_the_queued_page_runs_cancels_ingest() {
    let Some(test) = setup().await else { return };
    let csrf = login(&test.app, &test.world.admin).await;
    let profile = create_profile(
        &test.app,
        &csrf,
        test.world.mail_account_id,
        &format!("cancel-{}", Uuid::new_v4().simple()),
        SMTP_PASSWORD,
    )
    .await;
    let smtp_connection_id = Uuid::parse_str(profile["id"].as_str().unwrap()).unwrap();
    let enabled = put_inbox(
        &test.app,
        &csrf,
        smtp_connection_id,
        true,
        IMAP_HOST,
        "cancel-enable-key",
    )
    .await;
    assert_eq!(enabled.status(), StatusCode::OK);

    let source = Arc::new(FakeInboxSource {
        expected_password: SMTP_PASSWORD.into(),
        pages: Mutex::new(VecDeque::new()),
        calls: Mutex::new(Vec::new()),
        password_seen: AtomicBool::new(false),
    });
    let runner = runner(&test, source.clone());
    let disabled = put_inbox(
        &test.app,
        &csrf,
        smtp_connection_id,
        false,
        IMAP_HOST,
        "cancel-disable-key",
    )
    .await;
    assert_eq!(disabled.status(), StatusCode::OK);
    assert_eq!(
        runner
            .run_workspace_once(test.world.workspace_id)
            .await
            .unwrap(),
        0
    );
    assert!(source.calls.lock().unwrap().is_empty());
    assert_eq!(
        inbox_counts(
            &test.api_db,
            test.world.workspace_id,
            test.world.mail_account_id,
            "<unused@example.test>"
        )
        .await,
        (0, 0)
    );
}
