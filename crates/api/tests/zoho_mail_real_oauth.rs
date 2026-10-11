use api::{AppState, testing::TestApp};
use infrastructure::{
    config::Config,
    db::{Db, ROLE_API},
    providers::SecretValue,
    testing::{TEST_PASSWORD, TestWorld, seed_workspace},
};
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

async fn setup() -> Option<(TestApp, TestWorld, tempfile::TempDir)> {
    let url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Zoho Mail OAuth test");
            return None;
        }
    };
    let owner = Db::connect(&url, None).await.unwrap();
    owner.migrate().await.unwrap();
    let db = Db::connect(&url, Some(ROLE_API)).await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let directory = tempfile::tempdir().unwrap();
    let mut config = Config::test();
    config.zoho.fake_for_tests = false;
    config.zoho.mail_client_id = Some("1000.local-mail-client".into());
    config.zoho.mail_client_secret = Some(SecretValue::new("local-mail-secret"));
    config.zoho.mail_redirect_uri =
        Some("https://office.example/api/v1/integrations/zoho/callback".into());
    config.secrets_file = directory.path().join("secrets.json");
    let app = TestApp::spawn_with_state(AppState::with_fake_providers(
        db.with_default_workspace(Some(world.workspace_id)),
        Arc::new(config),
        None,
    ))
    .await;
    Some((app, world, directory))
}

async fn login(app: &TestApp, world: &TestWorld) -> String {
    let response = app
        .login(world.admin.email.as_deref().unwrap(), TEST_PASSWORD)
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    TestApp::csrf_token_from(&response).unwrap()
}

async fn start_mail_connection(app: &TestApp, csrf: &str) -> (Uuid, String) {
    let response = app
        .client
        .post(app.url("/api/v1/mail-accounts/connection-requests"))
        .header("x-csrf-token", csrf)
        .json(&json!({ "region": "com" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body: Value = response.json().await.unwrap();
    let data = &body["data"];
    let connection_id = Uuid::parse_str(data["connection_id"].as_str().unwrap()).unwrap();
    let auth_url = reqwest::Url::parse(data["authorization_url"].as_str().unwrap()).unwrap();
    let query: std::collections::HashMap<String, String> = auth_url
        .query_pairs()
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect();
    assert_eq!(
        query.get("scope").map(String::as_str),
        Some(
            "ZohoMail.messages.READ,ZohoMail.messages.CREATE,ZohoMail.accounts.READ,ZohoMail.folders.READ"
        )
    );
    let state = query.get("state").unwrap().clone();
    (connection_id, state)
}

#[tokio::test]
async fn live_mail_callback_binds_to_the_starting_session_and_claims_state_once() {
    let Some((app, world, _directory)) = setup().await else {
        return;
    };
    let csrf = login(&app, &world).await;
    let (connection_id, state) = start_mail_connection(&app, &csrf).await;

    let callback = format!("/api/v1/integrations/zoho/callback?state={state}&error=access_denied");
    let response = app.client.get(app.url(&callback)).send().await.unwrap();
    let problem = TestApp::problem(response, StatusCode::SERVICE_UNAVAILABLE).await;
    assert_eq!(problem["code"], "INTEGRATION_REAUTH_REQUIRED");

    let replay = app.client.get(app.url(&callback)).send().await.unwrap();
    let problem = TestApp::problem(replay, StatusCode::BAD_REQUEST).await;
    assert_eq!(problem["code"], "VALIDATION_FAILED");

    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let used_at: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
        "SELECT used_at FROM integration_oauth_requests
          WHERE workspace_id=$1 AND connection_id=$2",
    )
    .bind(world.workspace_id)
    .bind(connection_id)
    .fetch_optional(tx.conn())
    .await
    .unwrap()
    .flatten();
    assert!(used_at.is_some());
    let connection = infrastructure::repo::mailboxes::get_connection(&mut tx, connection_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(connection.status, "reauth_required");
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn live_mail_callback_rejects_a_different_session_without_consuming_state() {
    let Some((app, world, _directory)) = setup().await else {
        return;
    };
    let first_csrf = login(&app, &world).await;
    let (connection_id, state) = start_mail_connection(&app, &first_csrf).await;
    let _second_csrf = login(&app, &world).await;

    let callback = format!("/api/v1/integrations/zoho/callback?state={state}&error=access_denied");
    let response = app.client.get(app.url(&callback)).send().await.unwrap();
    let problem = TestApp::problem(response, StatusCode::BAD_REQUEST).await;
    assert_eq!(problem["code"], "VALIDATION_FAILED");

    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let used_at: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
        "SELECT used_at FROM integration_oauth_requests
          WHERE workspace_id=$1 AND connection_id=$2",
    )
    .bind(world.workspace_id)
    .bind(connection_id)
    .fetch_optional(tx.conn())
    .await
    .unwrap()
    .flatten();
    assert!(used_at.is_none());
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn live_mail_callback_rejects_expired_database_state() {
    let Some((app, world, _directory)) = setup().await else {
        return;
    };
    let csrf = login(&app, &world).await;
    let (connection_id, state) = start_mail_connection(&app, &csrf).await;
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    sqlx::query(
        "UPDATE integration_oauth_requests SET expires_at=now()-interval '1 second'
          WHERE workspace_id=$1 AND connection_id=$2",
    )
    .bind(world.workspace_id)
    .bind(connection_id)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let callback = format!("/api/v1/integrations/zoho/callback?state={state}&error=access_denied");
    let response = app.client.get(app.url(&callback)).send().await.unwrap();
    let problem = TestApp::problem(response, StatusCode::BAD_REQUEST).await;
    assert_eq!(problem["code"], "VALIDATION_FAILED");
}

/// Workspace with no server-side Zoho Mail client settings: the client is
/// entered through Settings instead.
async fn setup_unconfigured() -> Option<(TestApp, TestWorld, tempfile::TempDir)> {
    let url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Zoho Mail OAuth settings test");
            return None;
        }
    };
    let owner = Db::connect(&url, None).await.unwrap();
    owner.migrate().await.unwrap();
    let db = Db::connect(&url, Some(ROLE_API)).await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let directory = tempfile::tempdir().unwrap();
    let mut config = Config::test();
    config.zoho.fake_for_tests = false;
    config.zoho.mail_client_id = None;
    config.zoho.mail_client_secret = None;
    config.zoho.mail_redirect_uri =
        Some("https://office.example/api/v1/integrations/zoho/callback".into());
    config.secrets_file = directory.path().join("secrets.json");
    let app = TestApp::spawn_with_state(AppState::with_fake_providers(
        db.with_default_workspace(Some(world.workspace_id)),
        Arc::new(config),
        None,
    ))
    .await;
    Some((app, world, directory))
}

#[tokio::test]
async fn oauth_client_settings_saved_in_settings_enable_connection_start() {
    let Some((app, world, directory)) = setup_unconfigured().await else {
        return;
    };
    let csrf = login(&app, &world).await;

    // Unconfigured: starting a connection names the missing fields (422).
    let response = app
        .client
        .post(app.url("/api/v1/mail-accounts/connection-requests"))
        .header("x-csrf-token", &csrf)
        .json(&json!({ "region": "com" }))
        .send()
        .await
        .unwrap();
    let problem = TestApp::problem(response, StatusCode::UNPROCESSABLE_ENTITY).await;
    let fields: Vec<&str> = problem["violations"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|violation| violation["field"].as_str())
        .collect();
    assert!(fields.contains(&"client_id") && fields.contains(&"client_secret"));

    let response = app
        .client
        .get(app.url("/api/v1/mail-accounts/oauth-settings"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["data"]["configured"], false);
    assert_eq!(body["data"]["source"], "none");
    assert_eq!(body["data"]["has_secret"], false);

    // Invalid client ID and a missing secret are rejected field by field.
    let response = app
        .client
        .put(app.url("/api/v1/mail-accounts/oauth-settings"))
        .header("x-csrf-token", &csrf)
        .json(&json!({ "client_id": "not-a-zoho-client" }))
        .send()
        .await
        .unwrap();
    let problem = TestApp::problem(response, StatusCode::BAD_REQUEST).await;
    let fields: Vec<&str> = problem["violations"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|violation| violation["field"].as_str())
        .collect();
    assert!(fields.contains(&"client_id") && fields.contains(&"client_secret"));

    // A plain-HTTP, non-local callback is refused.
    let response = app
        .client
        .put(app.url("/api/v1/mail-accounts/oauth-settings"))
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "client_id": "1000.SETTINGSCLIENT",
            "client_secret": "settings-secret-value",
            "redirect_uri": "http://attacker.example/callback",
        }))
        .send()
        .await
        .unwrap();
    let problem = TestApp::problem(response, StatusCode::BAD_REQUEST).await;
    assert!(
        problem["violations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|violation| violation["field"] == "redirect_uri")
    );

    // Valid settings are saved; the secret is write-only.
    let response = app
        .client
        .put(app.url("/api/v1/mail-accounts/oauth-settings"))
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "client_id": "1000.SETTINGSCLIENT",
            "client_secret": "settings-secret-value",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let raw = response.text().await.unwrap();
    assert!(!raw.contains("settings-secret-value"));
    let body: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(body["data"]["configured"], true);
    assert_eq!(body["data"]["source"], "workspace");
    assert_eq!(body["data"]["client_id"], "1000.SETTINGSCLIENT");
    assert_eq!(body["data"]["has_secret"], true);

    let stored = std::fs::read_to_string(directory.path().join("secrets.json")).unwrap();
    assert!(!stored.contains("settings-secret-value"));

    // Updating the client ID alone keeps the stored secret.
    let response = app
        .client
        .put(app.url("/api/v1/mail-accounts/oauth-settings"))
        .header("x-csrf-token", &csrf)
        .json(&json!({ "client_id": "1000.SECONDCLIENT" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["data"]["configured"], true);
    assert_eq!(body["data"]["has_secret"], true);

    // The saved client is used by the OAuth start without any server env.
    let response = app
        .client
        .post(app.url("/api/v1/mail-accounts/connection-requests"))
        .header("x-csrf-token", &csrf)
        .json(&json!({ "region": "com" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body: Value = response.json().await.unwrap();
    let auth_url =
        reqwest::Url::parse(body["data"]["authorization_url"].as_str().unwrap()).unwrap();
    let query: std::collections::HashMap<String, String> = auth_url
        .query_pairs()
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect();
    assert_eq!(
        query.get("client_id").map(String::as_str),
        Some("1000.SECONDCLIENT")
    );
    assert_eq!(
        query.get("redirect_uri").map(String::as_str),
        Some("https://office.example/api/v1/integrations/zoho/callback")
    );
}
