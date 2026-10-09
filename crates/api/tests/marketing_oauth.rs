use api::{AppState, testing::TestApp};
use infrastructure::providers::zoho_campaigns::{
    CampaignsOAuthConfig, SecretValue, http::CampaignsHttp,
};
use infrastructure::{
    config::Config,
    db::{Db, ROLE_API},
    testing::{TEST_PASSWORD, TestWorld, seed_workspace},
};
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::sync::Arc;

async fn setup(
    configured: bool,
) -> (
    TestApp,
    TestWorld,
    tempfile::TempDir,
    tokio::task::JoinHandle<()>,
) {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL required");
    let owner = Db::connect(&url, None).await.unwrap();
    owner.migrate().await.unwrap();
    let db = Db::connect(&url, Some(ROLE_API)).await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::test();
    config.secrets_file = dir.path().join("secrets.json");
    config.zoho.fake_for_tests = false;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let router=axum::Router::new().route("/oauth/v2/token",axum::routing::post(||async{axum::Json(json!({"access_token":"private-access","refresh_token":"private-refresh","expires_in":3600}))}))
      .route("/api/v1.1/getmailinglists",axum::routing::get(||async{axum::Json(json!({"code":"0","list_of_details":[]}))}));
    let handle = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    if configured {
        config.zoho.campaigns = CampaignsOAuthConfig {
            allow_local_redirect: true,
            client_id: Some("1000.test".into()),
            client_secret: Some(SecretValue::new("private-secret")),
            redirect_uri: Some(
                "http://localhost:3000/api/v1/integrations/zoho-campaigns/callback".into(),
            ),
        };
    }
    let provider = if configured {
        Some(Arc::new(
            CampaignsHttp::for_test(config.zoho.campaigns.clone(), &base).unwrap(),
        ))
    } else {
        None
    };
    let mut state = AppState::with_fake_providers(
        db.with_default_workspace(Some(world.workspace_id)),
        Arc::new(config),
        None,
    );
    state.campaigns = provider;
    (TestApp::spawn_with_state(state).await, world, dir, handle)
}
async fn login(app: &TestApp, world: &TestWorld) -> String {
    let response = app
        .login(world.admin.email.as_deref().unwrap(), TEST_PASSWORD)
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    TestApp::csrf_token_from(&response).unwrap()
}
async fn connect(app: &TestApp, csrf: &str, key: &str) -> reqwest::Response {
    app.client
        .post(app.url("/api/v1/integrations/zoho-campaigns/connect"))
        .header("x-csrf-token", csrf)
        .header("idempotency-key", key)
        .json(&json!({}))
        .send()
        .await
        .unwrap()
}
#[tokio::test]
async fn missing_oauth_configuration_is_explained_and_does_not_emit_invalid_client_url() {
    let (app, world, _dir, handle) = setup(false).await;
    let csrf = login(&app, &world).await;
    let response = app
        .client
        .get(app.url("/api/v1/integrations/zoho-campaigns"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let value: Value = response.json().await.unwrap();
    assert_eq!(value["data"]["configured"], false);
    assert_eq!(value["data"]["scheduled_send"], false);
    assert!(
        value["data"]["missing_fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "ZOHO_CAMPAIGNS_CLIENT_ID")
    );
    let response = connect(&app, &csrf, &uuid::Uuid::new_v4().to_string()).await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!response.text().await.unwrap().contains("authorization_url"));
    let response = app
        .post_with_csrf(
            "/api/v1/mail-accounts/connection-requests",
            json!({"region":"com"}),
            &csrf,
        )
        .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    handle.abort();
}
#[tokio::test]
async fn campaigns_oauth_is_single_use_binds_session_and_stores_only_encrypted_tokens() {
    let (app, world, dir, handle) = setup(true).await;
    let csrf = login(&app, &world).await;
    let key = uuid::Uuid::new_v4().to_string();
    let response = connect(&app, &csrf, &key).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let data = TestApp::data(response).await;
    let id = data["connection_id"].as_str().unwrap();
    let url = reqwest::Url::parse(data["authorization_url"].as_str().unwrap()).unwrap();
    let state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned();
    assert_eq!(
        url.query_pairs().find(|(k, _)| k == "client_id").unwrap().1,
        "1000.test"
    );
    let replay = TestApp::data(connect(&app, &csrf, &key).await).await;
    assert!(replay["authorization_url"].is_null());
    assert_eq!(replay["connection_id"], id);
    let (other, other_world, _other_dir, other_handle) = setup(true).await;
    login(&other, &other_world).await;
    let callback = format!(
        "/api/v1/integrations/zoho-campaigns/callback?state={state}&code=private-code&location=us"
    );
    assert_eq!(
        other
            .client
            .get(other.url(&callback))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let response = app.client.get(app.url(&callback)).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers()["referrer-policy"], "no-referrer");
    assert!(
        !response.headers()["location"]
            .to_str()
            .unwrap()
            .contains("private-code")
    );
    let setup: Value = app
        .client
        .get(app.url("/api/v1/integrations/zoho-campaigns"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(setup["data"]["connections"][0]["status"], "active");
    assert!(!setup.to_string().contains("private-"));
    let raw = std::fs::read_to_string(dir.path().join("secrets.json")).unwrap();
    assert!(!raw.contains("private-"));
    assert_eq!(
        app.client
            .get(app.url(&callback))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let persisted:String=sqlx::query_scalar("SELECT response_body::text FROM idempotency_records WHERE workspace_id=$1 AND idempotency_key=$2").bind(world.workspace_id).bind(&key).fetch_one(tx.conn()).await.unwrap();
    assert!(!persisted.contains(&state));
    assert!(!persisted.contains("private-"));
    tx.commit().await.unwrap();
    handle.abort();
    other_handle.abort();
}

#[tokio::test]
async fn agents_and_non_admins_cannot_read_server_setup_and_denials_are_audited() {
    let (app, world, _dir, handle) = setup(true).await;
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let (_, token) = infrastructure::repo::auth::create_service_credential(
        &mut tx,
        world.agent.principal_id,
        "Campaigns denial test",
        &["operations.read".into()],
        chrono::Duration::hours(1),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let response = reqwest::Client::new()
        .get(app.url("/api/v1/integrations/zoho-campaigns"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(!response.text().await.unwrap().contains("missing_fields"));
    let response = app
        .login(world.member_a.email.as_deref().unwrap(), TEST_PASSWORD)
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        app.client
            .get(app.url("/api/v1/integrations/zoho-campaigns"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE workspace_id=$1 AND action='marketing.oauth_setup' AND details->>'outcome' LIKE 'denied:%'").bind(world.workspace_id).fetch_one(tx.conn()).await.unwrap();
    assert_eq!(count, 2);
    tx.commit().await.unwrap();
    handle.abort();
}
