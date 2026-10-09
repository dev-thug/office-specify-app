//! Acceptance coverage gaps for cross-workspace mailbox isolation and
//! grantless-admin mail search/export boundaries (T01, T02).

use api::AppState;
use api::testing::TestApp;
use chrono::Utc;
use infrastructure::config::Config;
use infrastructure::db::{Db, ROLE_API};
use infrastructure::providers::ProviderMessage;
use infrastructure::repo::auth as auth_repo;
use infrastructure::testing::{TEST_PASSWORD, TestPrincipal, TestWorld, seed_workspace};
use reqwest::StatusCode;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

async fn test_db() -> Option<Db> {
    let url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping acceptance-gap API test");
            return None;
        }
    };
    let owner = Db::connect(&url, None).await.unwrap();
    owner.migrate().await.unwrap();
    Some(Db::connect(&url, Some(ROLE_API)).await.unwrap())
}

async fn app_for(db: &Db, world: &TestWorld) -> TestApp {
    let db = db.clone().with_default_workspace(Some(world.workspace_id));
    TestApp::spawn_with_state(AppState::with_fake_providers(
        db,
        Arc::new(Config::test()),
        None,
    ))
    .await
}

async fn login(app: &TestApp, principal: &TestPrincipal) -> String {
    let response = app
        .login(principal.email.as_deref().unwrap(), TEST_PASSWORD)
        .await;
    assert_eq!(response.status(), StatusCode::OK, "login failed");
    TestApp::csrf_token_from(&response).expect("login did not set CSRF cookie")
}

async fn grantless_admin(app: &TestApp, world: &TestWorld) -> TestPrincipal {
    let email = format!("admin-no-mail-{}@test.dev", Uuid::new_v4().simple());
    let user = auth_repo::create_user(&app.state.db, &email, "Admin Without Mailbox Grant")
        .await
        .unwrap();
    let password_hash = infrastructure::crypto::hash_password(TEST_PASSWORD).unwrap();
    auth_repo::set_password_credential(&app.state.db, user.id, &password_hash)
        .await
        .unwrap();
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let principal_id = auth_repo::insert_principal(
        &mut tx,
        "human",
        Some(user.id),
        "Admin Without Mailbox Grant",
        "admin",
        "active",
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    TestPrincipal {
        principal_id,
        user_id: Some(user.id),
        email: Some(email),
        role: contracts::Role::Admin,
        kind: contracts::PrincipalKind::Human,
    }
}

async fn ingest_searchable_message(
    app: &TestApp,
    world: &TestWorld,
    marker: &str,
) -> (Uuid, String, String) {
    let subject = format!("Private mailbox subject {marker}");
    let body = format!("Private mailbox body {marker}");
    let message = ProviderMessage {
        provider_message_id: format!("acceptance-{marker}"),
        provider_thread_id: Some(format!("thread-{marker}")),
        internet_message_id: Some(format!("<acceptance-{marker}@test.dev>")),
        in_reply_to: None,
        references: vec![],
        from: format!("sender-{marker}@example.test"),
        to: vec!["shared@test.dev".into()],
        subject: Some(subject.clone()),
        body_text: Some(body.clone()),
        body_html: None,
        message_at: Utc::now(),
    };
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let ingested = application::inbox::ingest::ingest_provider_message(
        &mut tx,
        None,
        world.mail_account_id,
        &message,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    (ingested.thread_id, subject, body)
}

#[tokio::test]
async fn t01_cross_workspace_mail_account_detail_is_404() {
    let Some(db) = test_db().await else {
        return;
    };
    let world_a = seed_workspace(&db).await.unwrap();
    let world_b = seed_workspace(&db).await.unwrap();
    let app_a = app_for(&db, &world_a).await;
    let app_b = app_for(&db, &world_b).await;

    let _ = login(&app_a, &world_a.admin).await;
    let _ = login(&app_b, &world_b.admin).await;

    let response = app_b
        .get(&format!(
            "/api/v1/mail-accounts/{}",
            world_a.mail_account_id
        ))
        .await;
    let problem = TestApp::problem(response, StatusCode::NOT_FOUND).await;
    assert_eq!(problem["code"], "NOT_FOUND");

    let response = app_b.get("/api/v1/mail-accounts").await;
    let accounts = TestApp::data(response).await;
    assert!(
        accounts
            .as_array()
            .unwrap()
            .iter()
            .all(|account| account["id"] != world_a.mail_account_id.to_string()),
        "workspace B mailbox list must omit workspace A's account: {accounts}"
    );
}

#[tokio::test]
async fn t02_grantless_admin_mail_search_and_export_do_not_expose_content() {
    let Some(db) = test_db().await else {
        return;
    };
    let world = seed_workspace(&db).await.unwrap();
    let app = app_for(&db, &world).await;
    let (thread_id, subject, body) =
        ingest_searchable_message(&app, &world, &Uuid::new_v4().simple().to_string()).await;
    let search_term = subject.split_whitespace().last().unwrap();

    // The seeded admin's grant makes the exact subject searchable, proving
    // the message exists before the grantless-admin checks.
    let _ = login(&app, &world.admin).await;
    let response = app
        .get(&format!("/api/v1/email-threads?q={search_term}"))
        .await;
    let found = TestApp::data(response).await;
    assert!(
        found
            .as_array()
            .unwrap()
            .iter()
            .any(|thread| thread["id"] == thread_id.to_string()),
        "mailbox-granted admin should find the seeded subject: {found}"
    );

    let admin_without_grant = grantless_admin(&app, &world).await;
    let csrf = login(&app, &admin_without_grant).await;
    let response = app
        .get(&format!("/api/v1/email-threads?q={search_term}"))
        .await;
    let found = TestApp::data(response).await;
    assert_eq!(
        found.as_array().unwrap().len(),
        0,
        "grantless admin search must not expose the matching mailbox thread: {found}"
    );

    let response = app.get(&format!("/api/v1/email-threads/{thread_id}")).await;
    let problem = TestApp::problem(response, StatusCode::NOT_FOUND).await;
    let serialized_problem = problem.to_string();
    assert!(!serialized_problem.contains(&subject));
    assert!(!serialized_problem.contains(&body));

    // Export's public field allowlist is CRM-only; message bodies and
    // subjects cannot be requested through the CRM export job.
    let response = app
        .client
        .post(app.url("/api/v1/exports"))
        .header("x-csrf-token", &csrf)
        .header("idempotency-key", format!("mail-export-{}", Uuid::new_v4()))
        .json(&json!({
            "fields": ["message.subject", "message.body_text"],
            "filters": {"q": subject}
        }))
        .send()
        .await
        .unwrap();
    let problem = TestApp::problem(response, StatusCode::UNPROCESSABLE_ENTITY).await;
    assert_eq!(problem["code"], "VALIDATION_FAILED");
    let serialized = problem.to_string();
    assert!(
        !serialized.contains(&body),
        "mail export validation must not echo stored body content"
    );
}
