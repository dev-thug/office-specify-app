//! API security coverage for Office-owned Zoho Campaigns exports.

use api::{AppState, testing::TestApp};
use chrono::{Duration, Utc};
use infrastructure::{
    config::Config,
    db::{Db, ROLE_API},
    repo::{campaigns, marketing_exports},
    testing::{TestWorld, seed_workspace},
};
use reqwest::StatusCode;
use serde_json::json;
use std::sync::Arc;

async fn setup() -> Option<(TestApp, TestWorld)> {
    let url = std::env::var("DATABASE_URL").ok()?;
    let owner = Db::connect(&url, None).await.unwrap();
    owner.migrate().await.unwrap();
    let db = Db::connect(&url, Some(ROLE_API)).await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let mut config = Config::test();
    config.zoho.fake_for_tests = false;
    let state = AppState::with_fake_providers(
        db.with_default_workspace(Some(world.workspace_id)),
        Arc::new(config),
        None,
    );
    let app = TestApp::spawn_with_state(state).await;
    Some((app, world))
}

async fn seed_export(app: &TestApp, world: &TestWorld) -> uuid::Uuid {
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let connection_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO integration_connections
            (workspace_id, provider, region, secret_ref, scopes, status, connected_by)
         VALUES ($1,'zoho_campaigns','com','test-marketing-delivery',
                 ARRAY['ZohoCampaigns.campaign.READ','ZohoCampaigns.contact.READ'],
                 'active',$2) RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(world.admin.principal_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    sqlx::query("INSERT INTO marketing_connections (workspace_id,id) VALUES ($1,$2)")
        .bind(world.workspace_id)
        .bind(connection_id)
        .execute(tx.conn())
        .await
        .unwrap();
    let sender_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO sender_identities
            (workspace_id, mail_account_id, address, display_name, status, verified_at)
         VALUES ($1,$2,$3,'Marketing Sender','verified',now()) RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(world.mail_account_id)
    .bind(format!("sender+{}@example.test", uuid::Uuid::new_v4()))
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let channel_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO delivery_channels
            (workspace_id,mail_account_id,sender_identity_id,connection_id,
             provider,purpose,name,policy_status,enabled)
         VALUES ($1,$2,$3,$4,'zoho_campaigns','marketing','Zoho Campaigns',
                 'allowed',true) RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(world.mail_account_id)
    .bind(sender_id)
    .bind(connection_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let template_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO email_templates (workspace_id,name,created_by)
         VALUES ($1,'delivery fixture',$2) RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(world.admin.principal_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let template_version_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO template_versions
            (workspace_id,template_id,version_no,subject,body_text,body_html,
             variable_schema,content_hash,created_by)
         VALUES ($1,$2,1,'Subject','Body','<p>Body</p>','{}',decode(repeat('11',32),'hex'),$3)
         RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(template_id)
    .bind(world.admin.principal_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let campaign_id = campaigns::insert_campaign(
        &mut tx,
        world.mail_account_id,
        "Delivery fixture",
        None,
        world.admin.principal_id,
    )
    .await
    .unwrap();
    let revision_id = campaigns::insert_revision(
        &mut tx,
        campaign_id,
        world.mail_account_id,
        channel_id,
        1,
        "UTC",
        &json!({}),
        true,
        true,
        world.admin.principal_id,
    )
    .await
    .unwrap();
    let step_id = campaigns::insert_step(&mut tx, revision_id, template_version_id, 1, 0)
        .await
        .unwrap();
    let launch_id = campaigns::insert_launch(
        &mut tx,
        revision_id,
        channel_id,
        world.admin.principal_id,
        Utc::now() + Duration::days(1),
    )
    .await
    .unwrap();
    let export = marketing_exports::insert_export(
        &mut tx,
        marketing_exports::NewMarketingExport {
            launch_id,
            revision_id,
            step_id,
            delivery_channel_id: channel_id,
            connection_id,
            source_manifest_hash: vec![0x11; 32],
            content_hash: vec![0x22; 32],
            audience_hash: vec![0x33; 32],
            export_hash: vec![0x44; 32],
            subject: "Secret subject".into(),
            body_text: "Secret text".into(),
            body_html: "<p>Secret HTML</p>".into(),
            sender_address: "sender@example.test".into(),
            sender_name: "Sender".into(),
            field_map: json!({}),
            prepared_by: world.admin.principal_id,
            expires_at: Utc::now() + Duration::days(1),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    export.id
}

#[tokio::test]
async fn export_endpoints_deny_agent_credentials_even_for_cross_workspace_ids() {
    let Some((app, world)) = setup().await else {
        eprintln!("DATABASE_URL not set — skipping API test");
        return;
    };
    let foreign = seed_workspace(&app.state.db).await.unwrap();
    let mut tx = app.state.db.begin(foreign.workspace_id).await.unwrap();
    let (_, token) = infrastructure::repo::auth::create_service_credential(
        &mut tx,
        foreign.agent.principal_id,
        "marketing delivery denial test",
        &["mail.send".into(), "campaign.compose".into()],
        chrono::Duration::hours(1),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let foreign_db = app
        .state
        .db
        .clone()
        .with_default_workspace(Some(foreign.workspace_id));
    let foreign_state = AppState::with_fake_providers(foreign_db, app.state.config.clone(), None);
    let foreign_app = TestApp::spawn_with_state(foreign_state).await;
    let response = foreign_app
        .client
        .post(foreign_app.url("/api/v1/marketing/exports"))
        .bearer_auth(token)
        .header("idempotency-key", uuid::Uuid::new_v4().to_string())
        .json(&json!({
            "launch_id": world.workspace_id,
            "topic_id": null,
            "field_map": {}
        }))
        .send()
        .await
        .unwrap();
    let problem = TestApp::problem(response, StatusCode::FORBIDDEN).await;
    assert_eq!(problem["code"], "FORBIDDEN");
}

#[tokio::test]
async fn signed_content_route_hides_invalid_signatures_as_not_found() {
    let Some((app, world)) = setup().await else {
        eprintln!("DATABASE_URL not set — skipping API test");
        return;
    };
    let expires = chrono::Utc::now().timestamp() + 300;
    let path = format!(
        "/public/v1/marketing-content/{}/{}?expires={expires}&content_hash={}&signature={}",
        world.workspace_id,
        uuid::Uuid::new_v4(),
        "00".repeat(32),
        "invalid-signature"
    );
    let response = app.get(&path).await;
    let problem = TestApp::problem(response, StatusCode::NOT_FOUND).await;
    assert_eq!(problem["code"], "NOT_FOUND");
}

#[tokio::test]
async fn signed_content_route_serves_the_hash_bound_snapshot_with_browser_guards() {
    let Some((app, world)) = setup().await else {
        eprintln!("DATABASE_URL not set — skipping API test");
        return;
    };
    let export_id = seed_export(&app, &world).await;
    let expires = chrono::Utc::now().timestamp() + 300;
    let content_hash = "22".repeat(32);
    let message = format!(
        "marketing-content-v1\n{}\n{}\n{}\n{}",
        world.workspace_id, export_id, expires, content_hash
    );
    let signature = infrastructure::crypto::b64_encode(&infrastructure::crypto::hmac_sha256(
        &app.state.config.hmac_key,
        message.as_bytes(),
    ));
    let path = format!(
        "/public/v1/marketing-content/{}/{export_id}?expires={expires}&content_hash={content_hash}&signature={signature}",
        world.workspace_id
    );
    let response = app.get(&path).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "text/html; charset=utf-8"
    );
    assert_eq!(
        response.headers().get("x-content-type-options").unwrap(),
        "nosniff"
    );
    assert_eq!(
        response.headers().get("referrer-policy").unwrap(),
        "no-referrer"
    );
    assert_eq!(response.headers().get("cache-control").unwrap(), "no-store");
    assert!(
        response
            .headers()
            .get("content-security-policy")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("default-src 'none'")
    );
    assert_eq!(response.text().await.unwrap(), "<p>Secret HTML</p>");
}

#[tokio::test]
async fn campaign_export_detail_and_content_require_mailbox_read_grant() {
    let Some((app, world)) = setup().await else {
        eprintln!("DATABASE_URL not set — skipping API test");
        return;
    };
    let export_id = seed_export(&app, &world).await;
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    sqlx::query(
        "UPDATE mail_account_grants
            SET can_read=false,can_compose=false,can_send=false,
                can_approve=false,can_assign=false
          WHERE workspace_id=$1 AND mail_account_id=$2 AND principal_id=$3",
    )
    .bind(world.workspace_id)
    .bind(world.mail_account_id)
    .bind(world.admin.principal_id)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let login = app
        .login(
            world.admin.email.as_deref().unwrap(),
            infrastructure::testing::TEST_PASSWORD,
        )
        .await;
    assert_eq!(login.status(), StatusCode::OK);
    let detail = app
        .get(&format!("/api/v1/marketing/exports/{export_id}"))
        .await;
    let not_found = TestApp::problem(detail, StatusCode::NOT_FOUND).await;
    assert_eq!(not_found["code"], "NOT_FOUND");

    let content = app
        .get(&format!("/api/v1/marketing/exports/{export_id}/content"))
        .await;
    let not_found = TestApp::problem(content, StatusCode::NOT_FOUND).await;
    assert_eq!(not_found["code"], "NOT_FOUND");

    let list = app.get("/api/v1/marketing/exports").await;
    let data = TestApp::data(list).await;
    assert_eq!(data.as_array().unwrap().len(), 0);
}
