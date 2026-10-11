//! Personalized sales campaign API regressions. All writes use the dedicated
//! `outreach_sales_kanban_api` database and fake providers.

use api::{AppState, testing::TestApp};
use infrastructure::{
    config::Config,
    db::{Db, ROLE_API, ROLE_RETENTION},
    retention::{CacheInvalidator, ErasureRequest, erase_contact},
    storage::ObjectStore,
    testing::{TEST_PASSWORD, TestPrincipal, TestWorld, seed_workspace},
};
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

struct SalesCampaignTest {
    app: TestApp,
    world: TestWorld,
    csrf: String,
}

async fn setup() -> Option<SalesCampaignTest> {
    let url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping personalized sales API test");
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
        "personalized sales tests must use their dedicated database"
    );
    let owner = Db::connect(&url, None).await.unwrap();
    owner.migrate().await.unwrap();
    let db = Db::connect(&url, Some(ROLE_API)).await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    sqlx::query("UPDATE workspaces SET emergency_stop = false WHERE id = $1")
        .bind(world.workspace_id)
        .execute(tx.conn())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let app = TestApp::spawn_with_state(AppState::with_fake_providers(
        db.with_default_workspace(Some(world.workspace_id)),
        Arc::new(Config::test()),
        None,
    ))
    .await;
    let login = app
        .login(world.admin.email.as_deref().unwrap(), TEST_PASSWORD)
        .await;
    assert_eq!(login.status(), StatusCode::OK);
    let csrf = TestApp::csrf_token_from(&login).unwrap();
    Some(SalesCampaignTest { app, world, csrf })
}

async fn create_smtp_profile(test: &SalesCampaignTest, suffix: &str) -> (Uuid, Uuid, Uuid, String) {
    let mut tx = test
        .app
        .state
        .db
        .begin(test.world.workspace_id)
        .await
        .unwrap();
    let connection_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO integration_connections
            (id, workspace_id, provider, region, secret_ref, status, connected_by)
         VALUES ($1,$2,'delivery_provider','smtp',$3,'active',$4)",
    )
    .bind(connection_id)
    .bind(test.world.workspace_id)
    .bind(format!("fake://smtp/{suffix}"))
    .bind(test.world.admin.principal_id)
    .execute(tx.conn())
    .await
    .unwrap();
    let sender_address = format!("sales-{suffix}@example.test");
    let sender_identity_id: Uuid = sqlx::query_scalar(
        "INSERT INTO sender_identities
            (workspace_id, mail_account_id, address, display_name, status, verified_at)
         VALUES ($1,$2,$3,$4,'verified',now()) RETURNING id",
    )
    .bind(test.world.workspace_id)
    .bind(test.world.mail_account_id)
    .bind(&sender_address)
    .bind(format!("Sales {suffix}"))
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let delivery_channel_id: Uuid = sqlx::query_scalar(
        "INSERT INTO delivery_channels
            (workspace_id, mail_account_id, sender_identity_id, connection_id,
             provider, purpose, name, policy_status, enabled)
         VALUES ($1,$2,$3,$4,'delivery_provider','marketing',$5,'allowed',true)
         RETURNING id",
    )
    .bind(test.world.workspace_id)
    .bind(test.world.mail_account_id)
    .bind(sender_identity_id)
    .bind(connection_id)
    .bind(format!("Sales channel {suffix}"))
    .fetch_one(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO smtp_connections
            (id, workspace_id, mail_account_id, sender_identity_id, channel_id, name,
             host, port, security, username, from_address, from_name, reply_to,
             status, has_password)
         VALUES ($1,$2,$3,$4,$5,$6,'smtp.example.test',587,'starttls','smtp-user',
                 $7,$8,$9,'active',true)",
    )
    .bind(connection_id)
    .bind(test.world.workspace_id)
    .bind(test.world.mail_account_id)
    .bind(sender_identity_id)
    .bind(delivery_channel_id)
    .bind(format!("Sales profile {suffix}"))
    .bind(&sender_address)
    .bind(format!("Sales {suffix}"))
    .bind(format!("replies-{suffix}@example.test"))
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    (
        connection_id,
        delivery_channel_id,
        sender_identity_id,
        sender_address,
    )
}

async fn create_crm_target(test: &SalesCampaignTest, suffix: &str) -> (Uuid, Uuid, Uuid) {
    let mut tx = test
        .app
        .state
        .db
        .begin(test.world.workspace_id)
        .await
        .unwrap();
    let company_id: Uuid = sqlx::query_scalar(
        "INSERT INTO companies (workspace_id, name, domain)
         VALUES ($1,$2,$3) RETURNING id",
    )
    .bind(test.world.workspace_id)
    .bind(format!("Company {suffix}"))
    .bind(format!("{suffix}.example.test"))
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let contact_id: Uuid = sqlx::query_scalar(
        "INSERT INTO contacts (workspace_id, company_id, name, status)
         VALUES ($1,$2,$3,'active') RETURNING id",
    )
    .bind(test.world.workspace_id)
    .bind(company_id)
    .bind(format!("Contact {suffix}"))
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let address = format!("person-{suffix}@example.test");
    let email_address_id: Uuid = sqlx::query_scalar(
        "INSERT INTO email_addresses (workspace_id, address_original, normalized_key)
         VALUES ($1,$2,$3) RETURNING id",
    )
    .bind(test.world.workspace_id)
    .bind(&address)
    .bind(address.to_ascii_lowercase())
    .fetch_one(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO contact_emails (workspace_id, contact_id, email_address_id, is_primary)
         VALUES ($1,$2,$3,true)",
    )
    .bind(test.world.workspace_id)
    .bind(contact_id)
    .bind(email_address_id)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    (company_id, contact_id, email_address_id)
}

async fn create_extra_smtp_profile_without_admin_grant(
    test: &SalesCampaignTest,
    suffix: &str,
) -> Uuid {
    let mut tx = test
        .app
        .state
        .db
        .begin(test.world.workspace_id)
        .await
        .unwrap();
    let inbox_connection: Uuid = sqlx::query_scalar(
        "INSERT INTO integration_connections
            (workspace_id, provider, region, secret_ref, status, connected_by)
         VALUES ($1,'zoho_mail','us',$2,'active',$3) RETURNING id",
    )
    .bind(test.world.workspace_id)
    .bind(format!("fake://zoho/{suffix}"))
    .bind(test.world.admin.principal_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let mailbox_id: Uuid = sqlx::query_scalar(
        "INSERT INTO mail_accounts
            (workspace_id, connection_id, provider_account_id, primary_address, name, status)
         VALUES ($1,$2,$3,$4,$5,'active') RETURNING id",
    )
    .bind(test.world.workspace_id)
    .bind(inbox_connection)
    .bind(format!("extra-{suffix}"))
    .bind(format!("inbox-{suffix}@test.dev"))
    .bind(format!("Extra mailbox {suffix}"))
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let smtp_connection_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO integration_connections
            (id, workspace_id, provider, region, secret_ref, status, connected_by)
         VALUES ($1,$2,'delivery_provider','smtp',$3,'active',$4)",
    )
    .bind(smtp_connection_id)
    .bind(test.world.workspace_id)
    .bind(format!("fake://smtp-extra/{suffix}"))
    .bind(test.world.admin.principal_id)
    .execute(tx.conn())
    .await
    .unwrap();
    let address = format!("extra-sales-{suffix}@example.test");
    let sender_id: Uuid = sqlx::query_scalar(
        "INSERT INTO sender_identities
            (workspace_id,mail_account_id,address,display_name,status,verified_at)
         VALUES ($1,$2,$3,$4,'verified',now()) RETURNING id",
    )
    .bind(test.world.workspace_id)
    .bind(mailbox_id)
    .bind(&address)
    .bind(format!("Extra Sales {suffix}"))
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
    .bind(test.world.workspace_id)
    .bind(mailbox_id)
    .bind(sender_id)
    .bind(smtp_connection_id)
    .bind(format!("Extra channel {suffix}"))
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
    .bind(smtp_connection_id)
    .bind(test.world.workspace_id)
    .bind(mailbox_id)
    .bind(sender_id)
    .bind(channel_id)
    .bind(format!("Extra profile {suffix}"))
    .bind(&address)
    .bind(format!("Extra Sales {suffix}"))
    .bind(format!("replies-extra-{suffix}@example.test"))
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    smtp_connection_id
}

async fn create_group(test: &SalesCampaignTest, name: &str, key: &str) -> Value {
    let response = post_idempotent(
        test,
        "/api/v1/sales-campaigns",
        json!({"name":name,"objective":"Book product demos","meeting_type_id":null}),
        key,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    response.json::<Value>().await.unwrap()["data"].clone()
}

async fn add_target(
    test: &SalesCampaignTest,
    campaign_id: &str,
    company_id: Uuid,
    contact_id: Uuid,
    email_address_id: Uuid,
    key: &str,
) -> reqwest::Response {
    post_idempotent(
        test,
        &format!("/api/v1/sales-campaigns/{campaign_id}/targets"),
        json!({"company_id":company_id,"contact_id":contact_id,"email_address_id":email_address_id}),
        key,
    )
    .await
}

async fn create_target_draft(
    test: &SalesCampaignTest,
    campaign_id: &str,
    target_id: &str,
    subject: &str,
    body_text: &str,
    key: &str,
) -> Value {
    let response = post_idempotent(
        test,
        &format!("/api/v1/sales-campaigns/{campaign_id}/targets/{target_id}/drafts"),
        json!({
            "subject":subject,
            "body_text":body_text,
            "body_html":null,
            "template_version_id":null,
            "variables":null,
            "explicit_follow_up":false
        }),
        key,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    response.json::<Value>().await.unwrap()["data"].clone()
}

async fn put_sender(test: &SalesCampaignTest, profile_id: Uuid, key: &str) -> reqwest::Response {
    test.app
        .client
        .put(test.app.url("/api/v1/sales-campaigns/sender"))
        .header("x-csrf-token", &test.csrf)
        .header("Idempotency-Key", key)
        .json(&json!({"smtp_profile_id":profile_id}))
        .send()
        .await
        .unwrap()
}

async fn login_as(app: &TestApp, principal: &TestPrincipal) -> (reqwest::Client, String) {
    let client = reqwest::Client::builder()
        .cookie_store(true)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let response = client
        .post(app.url("/api/v1/auth/login"))
        .json(&json!({
            "email":principal.email.as_deref().unwrap(),
            "password":TEST_PASSWORD
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let csrf = TestApp::csrf_token_from(&response).unwrap();
    (client, csrf)
}

async fn insert_marketing_basis(test: &SalesCampaignTest, email_address_id: Uuid) {
    let mut tx = test
        .app
        .state
        .db
        .begin(test.world.workspace_id)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO communication_bases
            (workspace_id,email_address_id,purpose,jurisdiction,basis_type,status,
             evidence_ref,collected_at,reviewed_by,reviewed_at)
         VALUES ($1,$2,'marketing','US','explicit_consent','valid',$3,now(),$4,now())",
    )
    .bind(test.world.workspace_id)
    .bind(email_address_id)
    .bind(format!("test-consent:{}", Uuid::new_v4()))
    .bind(test.world.admin.principal_id)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

async fn freeze_draft(
    test: &SalesCampaignTest,
    draft_id: &str,
    channel_id: Uuid,
    sender_id: Uuid,
    key: &str,
) -> Value {
    let response = test
        .app
        .client
        .post(
            test.app
                .url(&format!("/api/v1/message-drafts/{draft_id}/freeze")),
        )
        .header("x-csrf-token", &test.csrf)
        .header("if-match", "\"1\"")
        .header("Idempotency-Key", key)
        .json(&json!({
            "delivery_channel_id":channel_id,
            "sender_identity_id":sender_id,
            "meeting_type_id":null,
            "tracking_requested":false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    response.json::<Value>().await.unwrap()["data"].clone()
}

async fn request_and_approve_outbound(
    test: &SalesCampaignTest,
    outbound_id: &str,
    payload_hash: &str,
) -> Value {
    let request = post_idempotent(
        test,
        &format!("/api/v1/outbound-messages/{outbound_id}/approval-requests"),
        json!({
            "reason":"Review company-specific sales message",
            "expected_payload_hash":payload_hash,
            "expires_at":null
        }),
        &format!("approval-request-{}", Uuid::new_v4().simple()),
    )
    .await;
    assert_eq!(request.status(), StatusCode::CREATED);
    let request = request.json::<Value>().await.unwrap()["data"].clone();
    let (approver, csrf) = login_as(&test.app, &test.world.manager).await;
    let decision = approver
        .post(test.app.url(&format!(
            "/api/v1/approvals/{}/decisions",
            request["approval_request_id"].as_str().unwrap()
        )))
        .header("x-csrf-token", csrf)
        .header(
            "if-match",
            format!("\"{}\"", request["version"].as_i64().unwrap()),
        )
        .header(
            "Idempotency-Key",
            format!("approval-decision-{}", Uuid::new_v4().simple()),
        )
        .json(&json!({
            "decision":"approved",
            "expected_snapshot_hash":payload_hash,
            "reason":"Reviewed exact company-specific snapshot"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(decision.status(), StatusCode::OK);
    TestApp::data(decision).await
}

async fn campaign_target_detail(test: &SalesCampaignTest, campaign_id: &str) -> Value {
    let response = test
        .app
        .get(&format!("/api/v1/sales-campaigns/{campaign_id}"))
        .await;
    TestApp::data(response).await
}

async fn create_service_token(test: &SalesCampaignTest, scopes: &[&str]) -> (Uuid, String) {
    let mut tx = test
        .app
        .state
        .db
        .begin(test.world.workspace_id)
        .await
        .unwrap();
    let result = infrastructure::repo::auth::create_service_credential(
        &mut tx,
        test.world.admin.principal_id,
        "sales campaign replay regression",
        &scopes
            .iter()
            .map(|scope| (*scope).to_string())
            .collect::<Vec<_>>(),
        chrono::Duration::hours(1),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    result
}

async fn queue_target(
    test: &SalesCampaignTest,
    campaign_id: &str,
    target_id: &str,
    outbound_id: &str,
    payload_hash: &str,
    target_version: i64,
    key: &str,
) -> reqwest::Response {
    post_idempotent(
        test,
        &format!("/api/v1/sales-campaigns/{campaign_id}/targets/{target_id}/queue"),
        json!({
            "outbound_message_id":outbound_id,
            "expected_payload_hash":payload_hash,
            "expected_target_version":target_version
        }),
        key,
    )
    .await
}

async fn post_idempotent(
    test: &SalesCampaignTest,
    path: &str,
    body: Value,
    key: &str,
) -> reqwest::Response {
    test.app
        .client
        .post(test.app.url(path))
        .header("x-csrf-token", &test.csrf)
        .header("Idempotency-Key", key)
        .json(&body)
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn one_group_keeps_company_drafts_separate_and_rejects_foreign_crm_bindings() {
    let Some(test) = setup().await else { return };
    let (profile_id, _, _, _) =
        create_smtp_profile(&test, &Uuid::new_v4().simple().to_string()).await;
    let (company_a, contact_a, email_a) =
        create_crm_target(&test, &Uuid::new_v4().simple().to_string()).await;
    let (company_b, contact_b, email_b) =
        create_crm_target(&test, &Uuid::new_v4().simple().to_string()).await;

    let created = post_idempotent(
        &test,
        "/api/v1/sales-campaigns",
        json!({"name":"Autumn outreach", "objective":"Book demos", "meeting_type_id":null}),
        "sales-group-create-001",
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let campaign = created.json::<Value>().await.unwrap()["data"].clone();
    let campaign_id = campaign["id"].as_str().unwrap();
    assert_eq!(campaign["smtp_profile_id"], profile_id.to_string());

    let target_a = post_idempotent(
        &test,
        &format!("/api/v1/sales-campaigns/{campaign_id}/targets"),
        json!({"company_id":company_a, "contact_id":contact_a, "email_address_id":email_a}),
        "sales-target-a-0001",
    )
    .await;
    assert_eq!(target_a.status(), StatusCode::CREATED);
    let target_a = target_a.json::<Value>().await.unwrap()["data"].clone();

    let target_b = post_idempotent(
        &test,
        &format!("/api/v1/sales-campaigns/{campaign_id}/targets"),
        json!({"company_id":company_b, "contact_id":contact_b, "email_address_id":email_b}),
        "sales-target-b-0001",
    )
    .await;
    assert_eq!(target_b.status(), StatusCode::CREATED);
    let target_b = target_b.json::<Value>().await.unwrap()["data"].clone();

    let invalid = post_idempotent(
        &test,
        &format!("/api/v1/sales-campaigns/{campaign_id}/targets"),
        json!({"company_id":company_a, "contact_id":contact_b, "email_address_id":email_b}),
        "sales-target-foreign-0001",
    )
    .await;
    assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);

    for (target, suffix) in [(&target_a, "Alpha"), (&target_b, "Beta")] {
        let target_id = target["id"].as_str().unwrap();
        let draft = post_idempotent(
            &test,
            &format!("/api/v1/sales-campaigns/{campaign_id}/targets/{target_id}/drafts"),
            json!({
                "subject":format!("Hello {suffix}"),
                "body_text":format!("Research-specific note for {suffix}."),
                "body_html":null,
                "template_version_id":null,
                "variables":null,
                "explicit_follow_up":false
            }),
            &format!("sales-draft-{suffix}-0001"),
        )
        .await;
        assert_eq!(draft.status(), StatusCode::CREATED);
        let draft = draft.json::<Value>().await.unwrap()["data"].clone();
        assert_eq!(draft["sales_campaign_id"], campaign_id);
        assert_eq!(draft["sales_target_id"], target_id);
        assert_eq!(draft["purpose"], "marketing");
        assert_eq!(draft["visibility"], "mailbox");
    }

    let detail = test
        .app
        .get(&format!("/api/v1/sales-campaigns/{campaign_id}"))
        .await
        .json::<Value>()
        .await
        .unwrap()["data"]
        .clone();
    let cards = detail["targets"].as_array().unwrap();
    assert_eq!(cards.len(), 2);
    let alpha = cards
        .iter()
        .find(|card| card["id"] == target_a["id"])
        .unwrap();
    let beta = cards
        .iter()
        .find(|card| card["id"] == target_b["id"])
        .unwrap();
    assert_eq!(alpha["current_draft"]["subject"], "Hello Alpha");
    assert_eq!(beta["current_draft"]["subject"], "Hello Beta");
    assert_eq!(alpha["stage"], "draft");
    assert_eq!(beta["stage"], "draft");

    let mut tx = test
        .app
        .state
        .db
        .begin(test.world.workspace_id)
        .await
        .unwrap();
    let recipient_bindings: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(
        "SELECT t.id, t.email_address_id, d.to_email_address_id
           FROM sales_campaign_targets t
           JOIN message_drafts d
             ON d.workspace_id = t.workspace_id AND d.id = t.current_draft_id
          WHERE t.workspace_id = $1 AND t.sales_campaign_id = $2
          ORDER BY t.id",
    )
    .bind(test.world.workspace_id)
    .bind(Uuid::parse_str(campaign_id).unwrap())
    .fetch_all(tx.conn())
    .await
    .unwrap();
    assert_eq!(recipient_bindings.len(), 2);
    for (target_id, target_email, draft_email) in recipient_bindings {
        assert_eq!(draft_email, target_email);
        if target_id == Uuid::parse_str(target_a["id"].as_str().unwrap()).unwrap() {
            assert_eq!(draft_email, email_a);
        } else {
            assert_eq!(draft_email, email_b);
        }
    }
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn default_sender_is_explicit_stable_and_a_disabled_default_does_not_fall_back() {
    let Some(test) = setup().await else { return };
    let suffix_a = Uuid::new_v4().simple().to_string();
    let suffix_b = Uuid::new_v4().simple().to_string();
    let (profile_a, _, _, from_a) = create_smtp_profile(&test, &suffix_a).await;
    let (profile_b, _, _, _) = create_smtp_profile(&test, &suffix_b).await;

    let set_a = put_sender(&test, profile_a, "sales-sender-a-0001").await;
    assert_eq!(set_a.status(), StatusCode::OK);
    let sender_a = set_a.json::<Value>().await.unwrap()["data"]["sender"].clone();
    assert_eq!(sender_a["smtp_profile_id"], profile_a.to_string());
    assert!(sender_a["ready"].as_bool().unwrap());
    assert!(sender_a["reply_to"].is_string());

    let campaign_a = create_group(&test, "Pinned sender A", "sales-campaign-a-0001").await;
    assert_eq!(campaign_a["smtp_profile_id"], profile_a.to_string());
    assert_eq!(campaign_a["from_address"], from_a);

    let set_b = put_sender(&test, profile_b, "sales-sender-b-0001").await;
    assert_eq!(set_b.status(), StatusCode::OK);
    // A campaign that has produced no message yet follows the new default.
    let follows_b = campaign_target_detail(&test, campaign_a["id"].as_str().unwrap()).await;
    assert_eq!(
        follows_b["campaign"]["smtp_profile_id"],
        profile_b.to_string()
    );
    assert_ne!(follows_b["campaign"]["from_address"], from_a);

    let campaign_b = create_group(&test, "Pinned sender B", "sales-campaign-b-0001").await;
    assert_eq!(campaign_b["smtp_profile_id"], profile_b.to_string());

    let mut tx = test
        .app
        .state
        .db
        .begin(test.world.workspace_id)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE smtp_connections SET status = 'disabled' WHERE workspace_id = $1 AND id = $2",
    )
    .bind(test.world.workspace_id)
    .bind(profile_b)
    .execute(tx.conn())
    .await
    .unwrap();
    let channel_b: Uuid = sqlx::query_scalar(
        "SELECT channel_id FROM smtp_connections WHERE workspace_id = $1 AND id = $2",
    )
    .bind(test.world.workspace_id)
    .bind(profile_b)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    sqlx::query("UPDATE delivery_channels SET enabled = false WHERE workspace_id = $1 AND id = $2")
        .bind(test.world.workspace_id)
        .bind(channel_b)
        .execute(tx.conn())
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let sender_get = test.app.get("/api/v1/sales-campaigns/sender").await;
    assert_eq!(sender_get.status(), StatusCode::CONFLICT);

    let create_with_stale_default = post_idempotent(
        &test,
        "/api/v1/sales-campaigns",
        json!({"name":"Must not silently retarget","objective":null,"meeting_type_id":null}),
        "sales-stale-default-001",
    )
    .await;
    assert_eq!(create_with_stale_default.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn campaign_with_a_frozen_message_stays_pinned_when_the_default_sender_changes() {
    let Some(test) = setup().await else { return };
    let (profile_a, channel_a, sender_a, from_a) =
        create_smtp_profile(&test, &Uuid::new_v4().simple().to_string()).await;
    let (profile_b, _, _, _) =
        create_smtp_profile(&test, &Uuid::new_v4().simple().to_string()).await;
    assert_eq!(
        put_sender(&test, profile_a, "sales-pin-sender-a-001")
            .await
            .status(),
        StatusCode::OK
    );
    let campaign = create_group(&test, "Pinned once frozen", "sales-pin-campaign-001").await;
    let campaign_id = campaign["id"].as_str().unwrap();
    let (company_id, contact_id, email_address_id) =
        create_crm_target(&test, &Uuid::new_v4().simple().to_string()).await;
    insert_marketing_basis(&test, email_address_id).await;
    let target = add_target(
        &test,
        campaign_id,
        company_id,
        contact_id,
        email_address_id,
        "sales-pin-target-001",
    )
    .await
    .json::<Value>()
    .await
    .unwrap()["data"]
        .clone();
    let draft = create_target_draft(
        &test,
        campaign_id,
        target["id"].as_str().unwrap(),
        "Pinned subject",
        "Pinned body.",
        "sales-pin-draft-001",
    )
    .await;
    freeze_draft(
        &test,
        draft["draft_id"].as_str().unwrap(),
        channel_a,
        sender_a,
        "sales-pin-freeze-001",
    )
    .await;

    assert_eq!(
        put_sender(&test, profile_b, "sales-pin-sender-b-001")
            .await
            .status(),
        StatusCode::OK
    );
    let detail = campaign_target_detail(&test, campaign_id).await;
    assert_eq!(detail["campaign"]["smtp_profile_id"], profile_a.to_string());
    assert_eq!(detail["campaign"]["from_address"], from_a);

    // The explicit refresh is refused for the same reason.
    let refresh = test
        .app
        .client
        .post(test.app.url(&format!(
            "/api/v1/sales-campaigns/{campaign_id}/sender/refresh"
        )))
        .header("x-csrf-token", &test.csrf)
        .send()
        .await
        .unwrap();
    assert_eq!(refresh.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn direct_sales_draft_fills_recipient_variables_from_the_crm_at_freeze() {
    let Some(test) = setup().await else { return };
    let suffix = Uuid::new_v4().simple().to_string();
    let (profile, channel, sender, _) = create_smtp_profile(&test, &suffix).await;
    assert_eq!(
        put_sender(&test, profile, "sales-vars-sender-001")
            .await
            .status(),
        StatusCode::OK
    );
    let campaign = create_group(&test, "Recipient variables", "sales-vars-campaign-001").await;
    let campaign_id = campaign["id"].as_str().unwrap();
    let (company_id, contact_id, email_address_id) = create_crm_target(&test, &suffix).await;
    insert_marketing_basis(&test, email_address_id).await;
    let target = add_target(
        &test,
        campaign_id,
        company_id,
        contact_id,
        email_address_id,
        "sales-vars-target-001",
    )
    .await
    .json::<Value>()
    .await
    .unwrap()["data"]
        .clone();
    let draft = create_target_draft(
        &test,
        campaign_id,
        target["id"].as_str().unwrap(),
        "{{company.name}} 제안",
        "{{ contact.name }}님, {{sender.display_name}}입니다.",
        "sales-vars-draft-001",
    )
    .await;
    let frozen = freeze_draft(
        &test,
        draft["draft_id"].as_str().unwrap(),
        channel,
        sender,
        "sales-vars-freeze-001",
    )
    .await;

    let mut tx = test
        .app
        .state
        .db
        .begin(test.world.workspace_id)
        .await
        .unwrap();
    let (subject, body): (String, String) = sqlx::query_as(
        "SELECT subject, body_text FROM outbound_messages WHERE workspace_id = $1 AND id = $2",
    )
    .bind(test.world.workspace_id)
    .bind(Uuid::parse_str(frozen["outbound_message_id"].as_str().unwrap()).unwrap())
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(subject, format!("Company {suffix} 제안"));
    assert!(body.starts_with(&format!("Contact {suffix}님, ")), "{body}");
    assert!(!body.contains("{{"), "{body}");
}

#[tokio::test]
async fn sender_get_resolves_the_sole_globally_eligible_profile_without_a_default() {
    let Some(test) = setup().await else { return };
    let (profile_id, _, _, _) =
        create_smtp_profile(&test, &Uuid::new_v4().simple().to_string()).await;

    let response = test.app.get("/api/v1/sales-campaigns/sender").await;
    assert_eq!(response.status(), StatusCode::OK);
    let sender = TestApp::data(response).await["sender"].clone();
    assert_eq!(sender["smtp_profile_id"], profile_id.to_string());
    assert_eq!(sender["ready"], true);

    let campaign = create_group(
        &test,
        "Sole default fallback",
        "sales-sole-fallback-group-001",
    )
    .await;
    assert_eq!(campaign["smtp_profile_id"], profile_id.to_string());
}

#[tokio::test]
async fn create_replay_authorizes_its_original_pinned_mailbox_after_default_changes() {
    let Some(test) = setup().await else { return };
    let (profile_a, _, _, _) =
        create_smtp_profile(&test, &Uuid::new_v4().simple().to_string()).await;
    let set_a = put_sender(&test, profile_a, "sales-replay-set-a-001").await;
    assert_eq!(set_a.status(), StatusCode::OK);
    let request = json!({
        "name":"Pinned create replay",
        "objective":"The response keeps the original sender binding",
        "meeting_type_id":null
    });
    let first = post_idempotent(
        &test,
        "/api/v1/sales-campaigns",
        request.clone(),
        "sales-pinned-create-replay-001",
    )
    .await;
    assert_eq!(first.status(), StatusCode::CREATED);
    let original = first.json::<Value>().await.unwrap()["data"].clone();

    let profile_b =
        create_extra_smtp_profile_without_admin_grant(&test, &Uuid::new_v4().simple().to_string())
            .await;
    let mut tx = test
        .app
        .state
        .db
        .begin(test.world.workspace_id)
        .await
        .unwrap();
    let mailbox_b: Uuid = sqlx::query_scalar(
        "SELECT mail_account_id FROM smtp_connections
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(test.world.workspace_id)
    .bind(profile_b)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO mail_account_grants
            (workspace_id,mail_account_id,principal_id,can_read,can_compose,can_send,
             can_approve,can_assign,granted_by)
         VALUES ($1,$2,$3,true,true,true,true,true,$3)",
    )
    .bind(test.world.workspace_id)
    .bind(mailbox_b)
    .bind(test.world.admin.principal_id)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let set_b = put_sender(&test, profile_b, "sales-replay-set-b-001").await;
    assert_eq!(set_b.status(), StatusCode::OK);

    let mut tx = test
        .app
        .state
        .db
        .begin(test.world.workspace_id)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE mail_account_grants
            SET can_read = false, can_compose = false, can_send = false,
                can_approve = false, can_assign = false, version = version + 1
          WHERE workspace_id = $1 AND mail_account_id = $2 AND principal_id = $3",
    )
    .bind(test.world.workspace_id)
    .bind(mailbox_b)
    .bind(test.world.admin.principal_id)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let replay = post_idempotent(
        &test,
        "/api/v1/sales-campaigns",
        request,
        "sales-pinned-create-replay-001",
    )
    .await;
    assert_eq!(replay.status(), StatusCode::CREATED);
    let replay = replay.json::<Value>().await.unwrap()["data"].clone();
    assert_eq!(replay["id"], original["id"]);
    assert_eq!(replay["smtp_profile_id"], profile_a.to_string());
}

#[tokio::test]
async fn sender_set_replay_rechecks_resource_access_without_revalidating_old_readiness() {
    let Some(test) = setup().await else { return };
    let (profile_id, channel_id, _, _) =
        create_smtp_profile(&test, &Uuid::new_v4().simple().to_string()).await;
    let key = "sales-setter-historical-replay-001";
    let first = put_sender(&test, profile_id, key).await;
    assert_eq!(first.status(), StatusCode::OK);

    let mut tx = test
        .app
        .state
        .db
        .begin(test.world.workspace_id)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE smtp_connections SET status = 'disabled' WHERE workspace_id = $1 AND id = $2",
    )
    .bind(test.world.workspace_id)
    .bind(profile_id)
    .execute(tx.conn())
    .await
    .unwrap();
    sqlx::query("UPDATE delivery_channels SET enabled = false WHERE workspace_id = $1 AND id = $2")
        .bind(test.world.workspace_id)
        .bind(channel_id)
        .execute(tx.conn())
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let replay = put_sender(&test, profile_id, key).await;
    assert_eq!(replay.status(), StatusCode::OK);
    let replay = replay.json::<Value>().await.unwrap()["data"]["sender"].clone();
    assert_eq!(replay["smtp_profile_id"], profile_id.to_string());
}

#[tokio::test]
async fn ambiguous_global_sender_set_requires_workspace_default_before_acl_filtering() {
    let Some(test) = setup().await else { return };
    let suffix = Uuid::new_v4().simple().to_string();
    let _accessible = create_smtp_profile(&test, &suffix).await;
    let _inaccessible =
        create_extra_smtp_profile_without_admin_grant(&test, &Uuid::new_v4().simple().to_string())
            .await;

    let sender_get = test.app.get("/api/v1/sales-campaigns/sender").await;
    assert_eq!(sender_get.status(), StatusCode::CONFLICT);

    let response = post_idempotent(
        &test,
        "/api/v1/sales-campaigns",
        json!({"name":"Ambiguous sender","objective":null,"meeting_type_id":null}),
        "sales-ambiguous-sender-001",
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let problem = response.json::<Value>().await.unwrap();
    assert!(
        problem["detail"]
            .as_str()
            .unwrap()
            .contains("select a default")
    );
}

#[tokio::test]
async fn campaign_get_filters_current_mailbox_read_grants() {
    let Some(test) = setup().await else { return };
    let _profile = create_smtp_profile(&test, &Uuid::new_v4().simple().to_string()).await;
    let campaign = create_group(&test, "Read-scoped group", "sales-read-group-001").await;
    let (member, _) = login_as(&test.app, &test.world.member_b).await;
    let detail = member
        .get(test.app.url(&format!(
            "/api/v1/sales-campaigns/{}",
            campaign["id"].as_str().unwrap()
        )))
        .send()
        .await
        .unwrap();
    assert_eq!(detail.status(), StatusCode::FORBIDDEN);

    let list = member
        .get(test.app.url("/api/v1/sales-campaigns"))
        .send()
        .await
        .unwrap();
    let list = TestApp::data(list).await;
    assert!(list.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn cached_target_response_rechecks_current_credential_scope_and_mailbox_grant() {
    let Some(test) = setup().await else { return };
    let _profile = create_smtp_profile(&test, &Uuid::new_v4().simple().to_string()).await;
    let campaign = create_group(
        &test,
        "Credential replay guard",
        "sales-credential-group-001",
    )
    .await;
    let campaign_id = campaign["id"].as_str().unwrap();
    let (company_id, contact_id, email_address_id) =
        create_crm_target(&test, &Uuid::new_v4().simple().to_string()).await;
    let (credential_id, token) = create_service_token(
        &test,
        &["campaign.compose", "crm.read", "mail.compose", "mail.read"],
    )
    .await;
    let path = format!("/api/v1/sales-campaigns/{campaign_id}/targets");
    let body = json!({
        "company_id":company_id,
        "contact_id":contact_id,
        "email_address_id":email_address_id
    });
    let first = test
        .app
        .client
        .post(test.app.url(&path))
        .bearer_auth(&token)
        .header("Idempotency-Key", "sales-credential-replay-001")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::CREATED);
    let target_id = first.json::<Value>().await.unwrap()["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let stage_path = format!("/api/v1/sales-campaigns/{campaign_id}/targets/{target_id}");
    let stage_body = json!({"stage":"research","expected_target_version":1});
    let (_, no_read_token) =
        create_service_token(&test, &["campaign.compose", "crm.read", "mail.compose"]).await;
    let no_read_stage = test
        .app
        .client
        .patch(test.app.url(&stage_path))
        .bearer_auth(&no_read_token)
        .header("Idempotency-Key", "sales-stage-no-read-001")
        .json(&stage_body)
        .send()
        .await
        .unwrap();
    assert_eq!(no_read_stage.status(), StatusCode::FORBIDDEN);
    let no_read_draft = test
        .app
        .client
        .post(test.app.url(&format!("{stage_path}/drafts")))
        .bearer_auth(&no_read_token)
        .header("Idempotency-Key", "sales-draft-no-read-001")
        .json(&json!({
            "subject":"Hidden without mail.read",
            "body_text":"This body must not be created or replayed.",
            "body_html":null,
            "template_version_id":null,
            "variables":null,
            "explicit_follow_up":false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(no_read_draft.status(), StatusCode::FORBIDDEN);
    let stage = test
        .app
        .client
        .patch(test.app.url(&stage_path))
        .bearer_auth(&token)
        .header("Idempotency-Key", "sales-credential-stage-001")
        .json(&stage_body)
        .send()
        .await
        .unwrap();
    assert_eq!(stage.status(), StatusCode::OK);

    let mut tx = test
        .app
        .state
        .db
        .begin(test.world.workspace_id)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE service_credentials
            SET scopes = array_remove(scopes, 'crm.read')
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(test.world.workspace_id)
    .bind(credential_id)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let narrow_replay = test
        .app
        .client
        .post(test.app.url(&path))
        .bearer_auth(&token)
        .header("Idempotency-Key", "sales-credential-replay-001")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(narrow_replay.status(), StatusCode::FORBIDDEN);
    let narrow_body = narrow_replay.text().await.unwrap();
    assert!(!narrow_body.contains(email_address_id.to_string().as_str()));

    let detail_without_crm = test
        .app
        .client
        .get(
            test.app
                .url(&format!("/api/v1/sales-campaigns/{campaign_id}")),
        )
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(detail_without_crm.status(), StatusCode::FORBIDDEN);
    assert!(
        !detail_without_crm
            .text()
            .await
            .unwrap()
            .contains(&target_id)
    );

    let narrow_stage_replay = test
        .app
        .client
        .patch(test.app.url(&stage_path))
        .bearer_auth(&token)
        .header("Idempotency-Key", "sales-credential-stage-001")
        .json(&stage_body)
        .send()
        .await
        .unwrap();
    assert_eq!(narrow_stage_replay.status(), StatusCode::FORBIDDEN);
    let narrow_stage_body = narrow_stage_replay.text().await.unwrap();
    assert!(!narrow_stage_body.contains(email_address_id.to_string().as_str()));

    let mut tx = test
        .app
        .state
        .db
        .begin(test.world.workspace_id)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE service_credentials
            SET scopes = array_append(scopes, 'crm.read')
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(test.world.workspace_id)
    .bind(credential_id)
    .execute(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE mail_account_grants
            SET can_read = false, can_compose = false, can_send = false,
                can_approve = false, can_assign = false, version = version + 1
          WHERE workspace_id = $1 AND mail_account_id = $2 AND principal_id = $3",
    )
    .bind(test.world.workspace_id)
    .bind(test.world.mail_account_id)
    .bind(test.world.admin.principal_id)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let revoked_grant_replay = test
        .app
        .client
        .post(test.app.url(&path))
        .bearer_auth(&token)
        .header("Idempotency-Key", "sales-credential-replay-001")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(revoked_grant_replay.status(), StatusCode::FORBIDDEN);
    let revoked_body = revoked_grant_replay.text().await.unwrap();
    assert!(!revoked_body.contains(email_address_id.to_string().as_str()));
}

#[tokio::test]
async fn one_target_per_group_email_prevents_shared_address_duplicates() {
    let Some(test) = setup().await else { return };
    let _profile = create_smtp_profile(&test, &Uuid::new_v4().simple().to_string()).await;
    let campaign = create_group(&test, "No duplicate recipient", "sales-dedupe-group-001").await;
    let campaign_id = campaign["id"].as_str().unwrap();
    let (company_a, contact_a, email_id) =
        create_crm_target(&test, &Uuid::new_v4().simple().to_string()).await;
    let target = add_target(
        &test,
        campaign_id,
        company_a,
        contact_a,
        email_id,
        "sales-dedupe-first-001",
    )
    .await;
    assert_eq!(target.status(), StatusCode::CREATED);
    let target_id = target.json::<Value>().await.unwrap()["data"]["id"].clone();

    let replay = add_target(
        &test,
        campaign_id,
        company_a,
        contact_a,
        email_id,
        "sales-dedupe-first-001",
    )
    .await;
    assert_eq!(replay.status(), StatusCode::CREATED);
    assert_eq!(
        replay.json::<Value>().await.unwrap()["data"]["id"],
        target_id
    );

    let mut tx = test
        .app
        .state
        .db
        .begin(test.world.workspace_id)
        .await
        .unwrap();
    let company_b: Uuid =
        sqlx::query_scalar("INSERT INTO companies (workspace_id,name) VALUES ($1,$2) RETURNING id")
            .bind(test.world.workspace_id)
            .bind(format!("Shared-address company {}", Uuid::new_v4()))
            .fetch_one(tx.conn())
            .await
            .unwrap();
    let contact_b: Uuid = sqlx::query_scalar(
        "INSERT INTO contacts (workspace_id,company_id,name,status)
         VALUES ($1,$2,$3,'active') RETURNING id",
    )
    .bind(test.world.workspace_id)
    .bind(company_b)
    .bind(format!("Shared-address contact {}", Uuid::new_v4()))
    .fetch_one(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO contact_emails (workspace_id,contact_id,email_address_id,is_primary)
         VALUES ($1,$2,$3,true)",
    )
    .bind(test.world.workspace_id)
    .bind(contact_b)
    .bind(email_id)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let duplicate = add_target(
        &test,
        campaign_id,
        company_b,
        contact_b,
        email_id,
        "sales-dedupe-second-001",
    )
    .await;
    assert_eq!(duplicate.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn human_queue_needs_no_separate_approval_and_replays_the_persisted_batch() {
    let Some(test) = setup().await else { return };
    let (profile_id, channel_id, sender_id, _) =
        create_smtp_profile(&test, &Uuid::new_v4().simple().to_string()).await;
    let campaign = create_group(&test, "Approved batch", "sales-approved-group-001").await;
    assert_eq!(campaign["smtp_profile_id"], profile_id.to_string());
    let campaign_id = campaign["id"].as_str().unwrap();
    let (company_id, contact_id, email_address_id) =
        create_crm_target(&test, &Uuid::new_v4().simple().to_string()).await;
    insert_marketing_basis(&test, email_address_id).await;
    let target_response = add_target(
        &test,
        campaign_id,
        company_id,
        contact_id,
        email_address_id,
        "sales-approved-target-001",
    )
    .await;
    assert_eq!(target_response.status(), StatusCode::CREATED);
    let target = target_response.json::<Value>().await.unwrap()["data"].clone();
    let target_id = target["id"].as_str().unwrap();
    let draft = create_target_draft(
        &test,
        campaign_id,
        target_id,
        "Reviewed company note",
        "The exact reviewed message for this company.",
        "sales-approved-draft-001",
    )
    .await;
    let frozen = freeze_draft(
        &test,
        draft["draft_id"].as_str().unwrap(),
        channel_id,
        sender_id,
        "sales-approved-freeze-001",
    )
    .await;
    let outbound_id = frozen["outbound_message_id"].as_str().unwrap();
    let payload_hash = frozen["payload_hash"].as_str().unwrap();
    let current = campaign_target_detail(&test, campaign_id).await;
    let target_version = current["targets"][0]["version"].as_i64().unwrap();

    // A human queueing the card is the approval; no separate request is needed.
    let key = "sales-queue-approved-001";
    let first = queue_target(
        &test,
        campaign_id,
        target_id,
        outbound_id,
        payload_hash,
        target_version,
        key,
    )
    .await;
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    let first: Value = first.json().await.unwrap();
    let first_data = first["data"].clone();
    let batch_at = chrono::DateTime::parse_from_rfc3339(first_data["batch_at"].as_str().unwrap())
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert_eq!(batch_at.timestamp().rem_euclid(600), 0);
    assert!(batch_at > chrono::Utc::now());

    let replay = queue_target(
        &test,
        campaign_id,
        target_id,
        outbound_id,
        payload_hash,
        target_version,
        key,
    )
    .await;
    assert_eq!(replay.status(), StatusCode::ACCEPTED);
    let replay = replay.json::<Value>().await.unwrap()["data"].clone();
    assert_eq!(
        replay["submission"]["job_id"],
        first_data["submission"]["job_id"]
    );
    assert_eq!(replay["batch_at"], first_data["batch_at"]);

    let job_id = Uuid::parse_str(first_data["submission"]["job_id"].as_str().unwrap()).unwrap();
    let outbound_id = Uuid::parse_str(outbound_id).unwrap();
    let mut tx = test
        .app
        .state
        .db
        .begin(test.world.workspace_id)
        .await
        .unwrap();
    let (not_before, job_time, send_attempts): (
        Option<chrono::DateTime<chrono::Utc>>,
        chrono::DateTime<chrono::Utc>,
        i64,
    ) = sqlx::query_as(
        "SELECT o.not_before, j.available_at,
                (SELECT count(*) FROM send_attempts a
                  WHERE a.workspace_id = o.workspace_id AND a.outbound_message_id = o.id)
           FROM outbound_messages o
           JOIN jobs j ON j.workspace_id = o.workspace_id AND j.id = $3
          WHERE o.workspace_id = $1 AND o.id = $2",
    )
    .bind(test.world.workspace_id)
    .bind(outbound_id)
    .bind(job_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(not_before, Some(batch_at));
    assert_eq!(job_time, batch_at);
    assert_eq!(send_attempts, 0, "API queueing does not call a provider");
    tx.rollback().await.unwrap();

    let board = campaign_target_detail(&test, campaign_id).await;
    assert_eq!(board["targets"][0]["stage"], "queued");
    assert_eq!(
        board["targets"][0]["outbound"]["not_before"],
        first_data["batch_at"]
    );
}

#[tokio::test]
async fn stale_outbound_cannot_queue_after_target_current_draft_changes() {
    let Some(test) = setup().await else { return };
    let (_, channel_id, sender_id, _) =
        create_smtp_profile(&test, &Uuid::new_v4().simple().to_string()).await;
    let campaign = create_group(&test, "Stale target", "sales-stale-group-001").await;
    let campaign_id = campaign["id"].as_str().unwrap();
    let (company_id, contact_id, email_address_id) =
        create_crm_target(&test, &Uuid::new_v4().simple().to_string()).await;
    insert_marketing_basis(&test, email_address_id).await;
    let target_response = add_target(
        &test,
        campaign_id,
        company_id,
        contact_id,
        email_address_id,
        "sales-stale-target-001",
    )
    .await;
    let target = target_response.json::<Value>().await.unwrap()["data"].clone();
    let target_id = target["id"].as_str().unwrap();
    let draft_a = create_target_draft(
        &test,
        campaign_id,
        target_id,
        "First draft",
        "First company-specific body.",
        "sales-stale-draft-a-001",
    )
    .await;
    let frozen_a = freeze_draft(
        &test,
        draft_a["draft_id"].as_str().unwrap(),
        channel_id,
        sender_id,
        "sales-stale-freeze-a-001",
    )
    .await;
    let draft_b = create_target_draft(
        &test,
        campaign_id,
        target_id,
        "Replacement draft",
        "Replacement body, independent of the previous one.",
        "sales-stale-draft-b-001",
    )
    .await;
    let version_b = campaign_target_detail(&test, campaign_id).await["targets"][0]["version"]
        .as_i64()
        .unwrap();
    assert_ne!(draft_a["draft_id"], draft_b["draft_id"]);

    let stale = queue_target(
        &test,
        campaign_id,
        target_id,
        frozen_a["outbound_message_id"].as_str().unwrap(),
        frozen_a["payload_hash"].as_str().unwrap(),
        version_b,
        "sales-stale-old-outbound-001",
    )
    .await;
    assert_eq!(stale.status(), StatusCode::CONFLICT);
    let board = campaign_target_detail(&test, campaign_id).await;
    assert_eq!(
        board["targets"][0]["current_draft"]["draft_id"],
        draft_b["draft_id"]
    );
    assert_eq!(board["targets"][0]["stage"], "draft");

    let mut tx = test
        .app
        .state
        .db
        .begin(test.world.workspace_id)
        .await
        .unwrap();
    let queued: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE workspace_id = $1
          AND kind = 'sending.dispatch_outbound' AND state = 'queued'",
    )
    .bind(test.world.workspace_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(queued, 0);
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn retention_erasure_clears_campaign_personal_links_without_breaking_the_group() {
    let Some(test) = setup().await else { return };
    let _profile = create_smtp_profile(&test, &Uuid::new_v4().simple().to_string()).await;
    let campaign = create_group(&test, "Retention-safe board", "sales-retention-group-001").await;
    let campaign_id = campaign["id"].as_str().unwrap();
    let (company_id, contact_id, email_address_id) =
        create_crm_target(&test, &Uuid::new_v4().simple().to_string()).await;
    let target = add_target(
        &test,
        campaign_id,
        company_id,
        contact_id,
        email_address_id,
        "sales-retention-target-001",
    )
    .await;
    let target = target.json::<Value>().await.unwrap()["data"].clone();
    let draft = create_target_draft(
        &test,
        campaign_id,
        target["id"].as_str().unwrap(),
        "Private company note",
        "Private draft body that must be scrubbed.",
        "sales-retention-draft-001",
    )
    .await;

    let retention_db = Db::connect(
        &std::env::var("DATABASE_URL").unwrap(),
        Some(ROLE_RETENTION),
    )
    .await
    .unwrap();
    let request = ErasureRequest {
        workspace_id: test.world.workspace_id,
        contact_id,
        actor_principal_id: Some(test.world.admin.principal_id),
        request_id: "sales-target-retention-test".into(),
        trace_id: None,
    };
    let report = erase_contact(&retention_db, request, &NoopObjectStore, &NoopCache)
        .await
        .unwrap();
    assert_eq!(report.contact_id, contact_id);

    let detail = campaign_target_detail(&test, campaign_id).await;
    let card = &detail["targets"][0];
    assert!(card["contact_id"].is_null());
    assert!(card["email_address_id"].is_null());
    assert!(card["contact_name"].is_null());
    assert!(card["email_address"].is_null());
    assert_eq!(card["company_id"], company_id.to_string());
    assert_eq!(card["stage"], "attention");
    assert_eq!(card["current_draft"]["draft_id"], draft["draft_id"]);
    assert_eq!(card["current_draft"]["subject"], "[erased]");
    assert!(card["current_draft"].get("body_text").is_none());
}

#[tokio::test]
async fn retention_scrubs_only_erased_contact_sales_drafts_and_cancels_its_queued_outbound() {
    let Some(test) = setup().await else { return };
    let (profile_id, channel_id, sender_id, _) =
        create_smtp_profile(&test, &Uuid::new_v4().simple().to_string()).await;
    let campaign_a = create_group(&test, "Erased contact group", "sales-shared-a-group-001").await;
    let campaign_b =
        create_group(&test, "Remaining contact group", "sales-shared-b-group-001").await;
    assert_eq!(campaign_a["smtp_profile_id"], profile_id.to_string());
    assert_eq!(campaign_b["smtp_profile_id"], profile_id.to_string());
    let campaign_a_id = campaign_a["id"].as_str().unwrap();
    let campaign_b_id = campaign_b["id"].as_str().unwrap();
    let (company_a, contact_a, shared_email_id) =
        create_crm_target(&test, &Uuid::new_v4().simple().to_string()).await;
    let (company_b, contact_b) = {
        let mut tx = test
            .app
            .state
            .db
            .begin(test.world.workspace_id)
            .await
            .unwrap();
        let company_id: Uuid = sqlx::query_scalar(
            "INSERT INTO companies (workspace_id,name) VALUES ($1,$2) RETURNING id",
        )
        .bind(test.world.workspace_id)
        .bind(format!("Shared recipient company {}", Uuid::new_v4()))
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let contact_id: Uuid = sqlx::query_scalar(
            "INSERT INTO contacts (workspace_id,company_id,name,status)
             VALUES ($1,$2,$3,'active') RETURNING id",
        )
        .bind(test.world.workspace_id)
        .bind(company_id)
        .bind(format!("Shared recipient contact {}", Uuid::new_v4()))
        .fetch_one(tx.conn())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO contact_emails (workspace_id,contact_id,email_address_id,is_primary)
             VALUES ($1,$2,$3,true)",
        )
        .bind(test.world.workspace_id)
        .bind(contact_id)
        .bind(shared_email_id)
        .execute(tx.conn())
        .await
        .unwrap();
        tx.commit().await.unwrap();
        (company_id, contact_id)
    };
    insert_marketing_basis(&test, shared_email_id).await;

    let target_a = add_target(
        &test,
        campaign_a_id,
        company_a,
        contact_a,
        shared_email_id,
        "sales-shared-a-target-001",
    )
    .await;
    assert_eq!(target_a.status(), StatusCode::CREATED);
    let target_a = target_a.json::<Value>().await.unwrap()["data"].clone();
    let target_b = add_target(
        &test,
        campaign_b_id,
        company_b,
        contact_b,
        shared_email_id,
        "sales-shared-b-target-001",
    )
    .await;
    assert_eq!(target_b.status(), StatusCode::CREATED);
    let target_b = target_b.json::<Value>().await.unwrap()["data"].clone();

    let draft_a = create_target_draft(
        &test,
        campaign_a_id,
        target_a["id"].as_str().unwrap(),
        "Erased private subject",
        "Erased private body",
        "sales-shared-a-draft-001",
    )
    .await;
    let draft_b = create_target_draft(
        &test,
        campaign_b_id,
        target_b["id"].as_str().unwrap(),
        "Remaining private subject",
        "Remaining private body",
        "sales-shared-b-draft-001",
    )
    .await;
    let frozen_a = freeze_draft(
        &test,
        draft_a["draft_id"].as_str().unwrap(),
        channel_id,
        sender_id,
        "sales-shared-a-freeze-001",
    )
    .await;
    let frozen_b = freeze_draft(
        &test,
        draft_b["draft_id"].as_str().unwrap(),
        channel_id,
        sender_id,
        "sales-shared-b-freeze-001",
    )
    .await;
    request_and_approve_outbound(
        &test,
        frozen_a["outbound_message_id"].as_str().unwrap(),
        frozen_a["payload_hash"].as_str().unwrap(),
    )
    .await;
    let before_erasure = campaign_target_detail(&test, campaign_a_id).await;
    let target_version = before_erasure["targets"][0]["version"].as_i64().unwrap();
    let queued = queue_target(
        &test,
        campaign_a_id,
        target_a["id"].as_str().unwrap(),
        frozen_a["outbound_message_id"].as_str().unwrap(),
        frozen_a["payload_hash"].as_str().unwrap(),
        target_version,
        "sales-shared-a-queue-001",
    )
    .await;
    assert_eq!(queued.status(), StatusCode::ACCEPTED);

    let retention_db = Db::connect(
        &std::env::var("DATABASE_URL").unwrap(),
        Some(ROLE_RETENTION),
    )
    .await
    .unwrap();
    erase_contact(
        &retention_db,
        ErasureRequest {
            workspace_id: test.world.workspace_id,
            contact_id: contact_a,
            actor_principal_id: Some(test.world.admin.principal_id),
            request_id: "sales-shared-contact-erasure".into(),
            trace_id: None,
        },
        &NoopObjectStore,
        &NoopCache,
    )
    .await
    .unwrap();

    let outbound_a = Uuid::parse_str(frozen_a["outbound_message_id"].as_str().unwrap()).unwrap();
    let outbound_b = Uuid::parse_str(frozen_b["outbound_message_id"].as_str().unwrap()).unwrap();
    let draft_a_id = Uuid::parse_str(draft_a["draft_id"].as_str().unwrap()).unwrap();
    let draft_b_id = Uuid::parse_str(draft_b["draft_id"].as_str().unwrap()).unwrap();
    let target_a_id = Uuid::parse_str(target_a["id"].as_str().unwrap()).unwrap();
    let queue_job_id: Uuid = queued.json::<Value>().await.unwrap()["data"]["submission"]["job_id"]
        .as_str()
        .map(Uuid::parse_str)
        .unwrap()
        .unwrap();
    let mut tx = test
        .app
        .state
        .db
        .begin(test.world.workspace_id)
        .await
        .unwrap();
    let erased_outbound: (
        String,
        String,
        String,
        Option<chrono::DateTime<chrono::Utc>>,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT status, subject, body_text, not_before, failure_code
               FROM outbound_messages WHERE workspace_id = $1 AND id = $2",
    )
    .bind(test.world.workspace_id)
    .bind(outbound_a)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(erased_outbound.0, "canceled");
    assert_eq!(erased_outbound.1, "[erased]");
    assert_eq!(erased_outbound.2, "");
    assert!(erased_outbound.3.is_none());
    assert_eq!(erased_outbound.4.as_deref(), Some("PRIVACY_ERASURE"));
    let remaining_outbound: (String, String, String) = sqlx::query_as(
        "SELECT status, subject, body_text FROM outbound_messages
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(test.world.workspace_id)
    .bind(outbound_b)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(remaining_outbound.0, "draft");
    assert_eq!(remaining_outbound.1, "Remaining private subject");
    assert!(remaining_outbound.2.contains("Remaining private body"));
    assert!(!remaining_outbound.2.contains("Erased private body"));
    let erased_draft: (String, String) = sqlx::query_as(
        "SELECT subject, body_text FROM message_drafts WHERE workspace_id = $1 AND id = $2",
    )
    .bind(test.world.workspace_id)
    .bind(draft_a_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(erased_draft, ("[erased]".into(), "".into()));
    let remaining_draft: (String, String) = sqlx::query_as(
        "SELECT subject, body_text FROM message_drafts WHERE workspace_id = $1 AND id = $2",
    )
    .bind(test.world.workspace_id)
    .bind(draft_b_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(remaining_draft.0, "Remaining private subject");
    assert_eq!(remaining_draft.1, "Remaining private body");
    let job_state: String =
        sqlx::query_scalar("SELECT state FROM jobs WHERE workspace_id = $1 AND id = $2")
            .bind(test.world.workspace_id)
            .bind(queue_job_id)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    assert_eq!(job_state, "canceled");
    let preserved_contact_link: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM contact_emails
          WHERE workspace_id = $1 AND contact_id = $2 AND email_address_id = $3)",
    )
    .bind(test.world.workspace_id)
    .bind(contact_b)
    .bind(shared_email_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert!(preserved_contact_link);
    let cached_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM idempotency_records
          WHERE workspace_id = $1 AND idempotency_key = ANY($2)",
    )
    .bind(test.world.workspace_id)
    .bind(vec![
        "sales-shared-a-target-001".to_string(),
        "sales-shared-a-draft-001".to_string(),
        "sales-shared-a-queue-001".to_string(),
    ])
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(cached_rows, 0);
    let remaining_cached_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM idempotency_records
          WHERE workspace_id = $1 AND idempotency_key = ANY($2)",
    )
    .bind(test.world.workspace_id)
    .bind(vec![
        "sales-shared-b-target-001".to_string(),
        "sales-shared-b-draft-001".to_string(),
    ])
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(remaining_cached_rows, 2);
    tx.rollback().await.unwrap();

    let erased_detail = campaign_target_detail(&test, campaign_a_id).await;
    let erased_card = &erased_detail["targets"][0];
    assert_eq!(erased_card["id"], target_a_id.to_string());
    assert_eq!(erased_card["stage"], "attention");
    assert!(erased_card["contact_id"].is_null());
    assert!(erased_card["email_address"].is_null());
    let remaining_detail = campaign_target_detail(&test, campaign_b_id).await;
    let remaining_card = &remaining_detail["targets"][0];
    assert_eq!(remaining_card["contact_id"], contact_b.to_string());
    assert_eq!(
        remaining_card["email_address_id"],
        shared_email_id.to_string()
    );
    assert_eq!(
        remaining_card["current_draft"]["subject"],
        "Remaining private subject"
    );

    let replay = add_target(
        &test,
        campaign_a_id,
        company_a,
        contact_a,
        shared_email_id,
        "sales-shared-a-target-001",
    )
    .await;
    assert!(matches!(
        replay.status(),
        StatusCode::CONFLICT | StatusCode::UNPROCESSABLE_ENTITY
    ));
    let replay_body = replay.text().await.unwrap();
    assert!(!replay_body.contains("Erased private subject"));
    assert!(!replay_body.contains(shared_email_id.to_string().as_str()));
}

#[tokio::test]
async fn target_add_racing_contact_erasure_finishes_without_a_stale_binding_or_replay() {
    let Some(test) = setup().await else { return };
    let _profile = create_smtp_profile(&test, &Uuid::new_v4().simple().to_string()).await;
    let campaign = create_group(&test, "Concurrent privacy gate", "sales-race-group-001").await;
    let campaign_id = campaign["id"].as_str().unwrap().to_string();
    let suffix = Uuid::new_v4().simple().to_string();
    let (company_id, contact_id, email_address_id) = create_crm_target(&test, &suffix).await;
    let email_address = format!("person-{suffix}@example.test");
    let retention_db = Db::connect(
        &std::env::var("DATABASE_URL").unwrap(),
        Some(ROLE_RETENTION),
    )
    .await
    .unwrap();
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let add_barrier = barrier.clone();
    let add_future = async {
        add_barrier.wait().await;
        tokio::time::timeout(
            std::time::Duration::from_secs(20),
            add_target(
                &test,
                &campaign_id,
                company_id,
                contact_id,
                email_address_id,
                "sales-race-add-001",
            ),
        )
        .await
    };
    let erase_barrier = barrier.clone();
    let erase_future = async {
        erase_barrier.wait().await;
        tokio::time::timeout(
            std::time::Duration::from_secs(20),
            erase_contact(
                &retention_db,
                ErasureRequest {
                    workspace_id: test.world.workspace_id,
                    contact_id,
                    actor_principal_id: Some(test.world.admin.principal_id),
                    request_id: "sales-add-erasure-race".into(),
                    trace_id: None,
                },
                &NoopObjectStore,
                &NoopCache,
            ),
        )
        .await
    };
    let (add_result, erase_result) = tokio::join!(add_future, erase_future);
    let add_result = add_result.expect("target add must not deadlock with erasure");
    let erase_result = erase_result.expect("contact erasure must not deadlock with target add");
    erase_result.unwrap();
    assert!(matches!(
        add_result.status(),
        StatusCode::CREATED | StatusCode::CONFLICT | StatusCode::UNPROCESSABLE_ENTITY
    ));

    let replay = add_target(
        &test,
        &campaign_id,
        company_id,
        contact_id,
        email_address_id,
        "sales-race-add-001",
    )
    .await;
    assert!(matches!(
        replay.status(),
        StatusCode::CONFLICT | StatusCode::UNPROCESSABLE_ENTITY
    ));
    let replay_body = replay.text().await.unwrap();
    assert!(!replay_body.contains(&email_address));

    let mut tx = test
        .app
        .state
        .db
        .begin(test.world.workspace_id)
        .await
        .unwrap();
    let live_target: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sales_campaign_targets
          WHERE workspace_id = $1 AND contact_id = $2 AND email_address_id = $3",
    )
    .bind(test.world.workspace_id)
    .bind(contact_id)
    .bind(email_address_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(live_target, 0);
    let cached_response: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM idempotency_records
          WHERE workspace_id = $1 AND idempotency_key = 'sales-race-add-001'",
    )
    .bind(test.world.workspace_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(cached_response, 0);
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn linked_draft_replacement_waits_for_campaign_before_locking_recipient() {
    let Some(test) = setup().await else { return };
    let (_, channel_id, sender_id, _) =
        create_smtp_profile(&test, &Uuid::new_v4().simple().to_string()).await;
    let campaign = create_group(&test, "Linked lock order", "sales-lock-order-group-001").await;
    let campaign_id = Uuid::parse_str(campaign["id"].as_str().unwrap()).unwrap();
    let (company_id, contact_id, email_address_id) =
        create_crm_target(&test, &Uuid::new_v4().simple().to_string()).await;
    insert_marketing_basis(&test, email_address_id).await;
    let target = add_target(
        &test,
        &campaign_id.to_string(),
        company_id,
        contact_id,
        email_address_id,
        "sales-lock-order-target-001",
    )
    .await;
    assert_eq!(target.status(), StatusCode::CREATED);
    let target_id = Uuid::parse_str(
        target.json::<Value>().await.unwrap()["data"]["id"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let draft = create_target_draft(
        &test,
        &campaign_id.to_string(),
        &target_id.to_string(),
        "Before lock order test",
        "The frozen draft exists to exercise linked submit ordering.",
        "sales-lock-order-draft-001",
    )
    .await;
    let frozen = freeze_draft(
        &test,
        draft["draft_id"].as_str().unwrap(),
        channel_id,
        sender_id,
        "sales-lock-order-freeze-001",
    )
    .await;
    let outbound_id = Uuid::parse_str(frozen["outbound_message_id"].as_str().unwrap()).unwrap();

    // Model the generic linked-submit order: campaign → target → outbound →
    // address. A concurrent draft replacement must wait on the campaign row
    // before it attempts the address row, otherwise this transaction and the
    // replacement can deadlock in opposite orders.
    let mut submit_like = test
        .app
        .state
        .db
        .begin(test.world.workspace_id)
        .await
        .unwrap();
    infrastructure::repo::sales_campaigns::lock_workspace_privacy_shared(&mut submit_like)
        .await
        .unwrap();
    infrastructure::repo::sales_campaigns::lock_campaign(&mut submit_like, campaign_id)
        .await
        .unwrap()
        .unwrap();
    infrastructure::repo::sales_campaigns::lock_target(&mut submit_like, campaign_id, target_id)
        .await
        .unwrap()
        .unwrap();
    infrastructure::repo::sending::get_outbound_for_update(&mut submit_like, outbound_id)
        .await
        .unwrap()
        .unwrap();

    let owner_db = Db::connect(&std::env::var("DATABASE_URL").unwrap(), None)
        .await
        .unwrap();
    let wait_started_at = chrono::Utc::now() - chrono::Duration::seconds(1);
    let client = test.app.client.clone();
    let draft_url = test.app.url(&format!(
        "/api/v1/sales-campaigns/{campaign_id}/targets/{target_id}/drafts"
    ));
    let csrf = test.csrf.clone();
    let replace = tokio::spawn(async move {
        client
            .post(draft_url)
            .header("x-csrf-token", csrf)
            .header("Idempotency-Key", "sales-lock-order-replace-001")
            .json(&json!({
                "subject":"After lock order test",
                "body_text":"The replacement draft follows the shared row-lock order.",
                "body_html":null,
                "template_version_id":null,
                "variables":null,
                "explicit_follow_up":false
            }))
            .send()
            .await
            .unwrap()
    });

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut waiting_for_campaign = false;
    while tokio::time::Instant::now() < deadline {
        waiting_for_campaign = sqlx::query_scalar(
            "SELECT EXISTS (
                SELECT 1 FROM pg_stat_activity
                 WHERE datname = current_database()
                   AND wait_event_type = 'Lock'
                   AND query LIKE 'SELECT id FROM sales_campaigns%'
                   AND xact_start >= $1)",
        )
        .bind(wait_started_at)
        .fetch_one(owner_db.pool())
        .await
        .unwrap();
        if waiting_for_campaign {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        waiting_for_campaign,
        "draft replacement should be waiting on campaign before touching recipient"
    );
    assert!(
        infrastructure::repo::compliance_core::lock_email_address(
            &mut submit_like,
            email_address_id
        )
        .await
        .unwrap()
    );
    submit_like.commit().await.unwrap();

    let replaced = tokio::time::timeout(std::time::Duration::from_secs(10), replace)
        .await
        .expect("draft replacement must not deadlock")
        .unwrap();
    assert_eq!(replaced.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn linked_draft_replacement_locks_campaign_target_before_recipient_address() {
    let Some(test) = setup().await else { return };
    let (_, channel_id, sender_id, _) =
        create_smtp_profile(&test, &Uuid::new_v4().simple().to_string()).await;
    let campaign = create_group(&test, "Linked lock order", "sales-lock-order-group-001").await;
    let campaign_id = campaign["id"].as_str().unwrap().to_string();
    let suffix = Uuid::new_v4().simple().to_string();
    let (company_id, contact_id, email_address_id) = create_crm_target(&test, &suffix).await;
    insert_marketing_basis(&test, email_address_id).await;
    let target = add_target(
        &test,
        &campaign_id,
        company_id,
        contact_id,
        email_address_id,
        "sales-lock-order-target-001",
    )
    .await;
    assert_eq!(target.status(), StatusCode::CREATED);
    let target_id = Uuid::parse_str(
        target.json::<Value>().await.unwrap()["data"]["id"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let draft = create_target_draft(
        &test,
        &campaign_id,
        &target_id.to_string(),
        "Before lock order test",
        "The frozen draft exists to exercise linked submit ordering.",
        "sales-lock-order-draft-001",
    )
    .await;
    let frozen = freeze_draft(
        &test,
        draft["draft_id"].as_str().unwrap(),
        channel_id,
        sender_id,
        "sales-lock-order-freeze-001",
    )
    .await;
    let campaign_id = Uuid::parse_str(&campaign_id).unwrap();
    let outbound_id = Uuid::parse_str(frozen["outbound_message_id"].as_str().unwrap()).unwrap();

    // Simulate the generic linked-submit lock order: campaign, target,
    // outbound, then recipient address. Draft replacement must wait on the
    // target before it attempts the address row.
    let mut submit_like = test
        .app
        .state
        .db
        .begin(test.world.workspace_id)
        .await
        .unwrap();
    infrastructure::repo::sales_campaigns::lock_workspace_privacy_shared(&mut submit_like)
        .await
        .unwrap();
    infrastructure::repo::sales_campaigns::lock_campaign(&mut submit_like, campaign_id)
        .await
        .unwrap()
        .unwrap();
    infrastructure::repo::sales_campaigns::lock_target(&mut submit_like, campaign_id, target_id)
        .await
        .unwrap()
        .unwrap();
    infrastructure::repo::sending::get_outbound_for_update(&mut submit_like, outbound_id)
        .await
        .unwrap()
        .unwrap();

    let owner_db = Db::connect(&std::env::var("DATABASE_URL").unwrap(), None)
        .await
        .unwrap();
    let wait_started_at = chrono::Utc::now() - chrono::Duration::seconds(1);
    let client = test.app.client.clone();
    let draft_url = test.app.url(&format!(
        "/api/v1/sales-campaigns/{campaign_id}/targets/{target_id}/drafts"
    ));
    let csrf = test.csrf.clone();
    let replace = tokio::spawn(async move {
        client
            .post(draft_url)
            .header("x-csrf-token", csrf)
            .header("Idempotency-Key", "sales-lock-order-replace-001")
            .json(&json!({
                "subject":"After lock order test",
                "body_text":"The replacement draft must follow the shared row-lock order.",
                "body_html":null,
                "template_version_id":null,
                "variables":null,
                "explicit_follow_up":false
            }))
            .send()
            .await
            .unwrap()
    });

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut waiting_for_campaign = false;
    while tokio::time::Instant::now() < deadline {
        waiting_for_campaign = sqlx::query_scalar(
            "SELECT EXISTS (
                SELECT 1 FROM pg_stat_activity
                 WHERE datname = current_database()
                   AND wait_event_type = 'Lock'
                   AND query LIKE 'SELECT id FROM sales_campaigns%'
                   AND xact_start >= $1)",
        )
        .bind(wait_started_at)
        .fetch_one(owner_db.pool())
        .await
        .unwrap();
        if waiting_for_campaign {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        waiting_for_campaign,
        "draft replacement should wait on campaign before touching the address"
    );
    assert!(
        infrastructure::repo::compliance_core::lock_email_address(
            &mut submit_like,
            email_address_id
        )
        .await
        .unwrap()
    );
    submit_like.commit().await.unwrap();

    let replaced = tokio::time::timeout(std::time::Duration::from_secs(10), replace)
        .await
        .expect("draft replacement must not deadlock")
        .unwrap();
    assert_eq!(replaced.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn group_detail_returns_all_two_hundred_targets_and_rejects_target_201() {
    let Some(test) = setup().await else { return };
    let _profile = create_smtp_profile(&test, &Uuid::new_v4().simple().to_string()).await;
    let campaign = create_group(&test, "Bounded target group", "sales-cap-group-001").await;
    let campaign_id = campaign["id"].as_str().unwrap();
    let (company_201, contact_201, email_201) =
        create_crm_target(&test, &Uuid::new_v4().simple().to_string()).await;

    let mut tx = test
        .app
        .state
        .db
        .begin(test.world.workspace_id)
        .await
        .unwrap();
    let inserted: i64 = sqlx::query_scalar(
        "WITH seed AS MATERIALIZED (
             SELECT n,
                    gen_random_uuid() AS company_id,
                    gen_random_uuid() AS contact_id,
                    gen_random_uuid() AS email_id,
                    gen_random_uuid() AS link_id,
                    'kanban-cap-' || n::text || '-' || substr(gen_random_uuid()::text,1,8)
                        || '@example.test' AS address
               FROM generate_series(1,200) AS n
         ), company_rows AS (
             INSERT INTO companies (id,workspace_id,name)
             SELECT company_id,$1,'Kanban cap company ' || n FROM seed
             RETURNING id
         ), contact_rows AS (
             INSERT INTO contacts (id,workspace_id,company_id,name,status)
             SELECT s.contact_id,$1,s.company_id,'Kanban cap contact ' || s.n,'active'
               FROM seed s JOIN company_rows c ON c.id = s.company_id
             RETURNING id
         ), address_rows AS (
             INSERT INTO email_addresses (id,workspace_id,address_original,normalized_key)
             SELECT s.email_id,$1,s.address,lower(s.address)
               FROM seed s JOIN contact_rows c ON c.id = s.contact_id
             RETURNING id
         ), link_rows AS (
             INSERT INTO contact_emails (id,workspace_id,contact_id,email_address_id)
             SELECT s.link_id,$1,s.contact_id,s.email_id
               FROM seed s JOIN address_rows a ON a.id = s.email_id
             RETURNING id
         ), target_rows AS (
             INSERT INTO sales_campaign_targets
                 (workspace_id,sales_campaign_id,company_id,contact_id,
                  contact_email_id,email_address_id)
             SELECT $1,$2,s.company_id,s.contact_id,l.id,s.email_id
               FROM seed s JOIN link_rows l ON l.id = s.link_id
             RETURNING id
         )
         SELECT count(*) FROM target_rows",
    )
    .bind(test.world.workspace_id)
    .bind(Uuid::parse_str(campaign_id).unwrap())
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(inserted, 200);
    tx.commit().await.unwrap();

    let detail = campaign_target_detail(&test, campaign_id).await;
    assert_eq!(detail["targets"].as_array().unwrap().len(), 200);

    let overflow = add_target(
        &test,
        campaign_id,
        company_201,
        contact_201,
        email_201,
        "sales-cap-overflow-001",
    )
    .await;
    assert_eq!(overflow.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let detail = campaign_target_detail(&test, campaign_id).await;
    assert_eq!(detail["targets"].as_array().unwrap().len(), 200);
}

struct NoopObjectStore;

#[async_trait::async_trait]
impl ObjectStore for NoopObjectStore {
    async fn put(&self, _key: &str, _plaintext: &[u8]) -> Result<(), contracts::AppError> {
        Ok(())
    }

    async fn get(&self, _key: &str) -> Result<Vec<u8>, contracts::AppError> {
        Err(contracts::AppError::not_found("object not found"))
    }

    async fn delete(&self, _key: &str) -> Result<(), contracts::AppError> {
        Ok(())
    }

    fn sign_access(&self, key: &str, _ttl: std::time::Duration) -> String {
        key.to_string()
    }

    fn verify_access(&self, key: &str, token: &str) -> bool {
        key == token
    }
}

struct NoopCache;

#[async_trait::async_trait]
impl CacheInvalidator for NoopCache {
    async fn invalidate_contact(
        &self,
        _workspace_id: Uuid,
        _contact_id: Uuid,
        _email_address_ids: &[Uuid],
    ) -> Result<(), contracts::AppError> {
        Ok(())
    }
}
