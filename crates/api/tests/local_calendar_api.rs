//! Local calendar connection API checks. Uses only the dedicated
//! `outreach_local_booking_backend` database.

use api::{AppState, testing::TestApp};
use infrastructure::{
    config::Config,
    db::{Db, ROLE_API},
    testing::{TEST_PASSWORD, seed_workspace},
};
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::sync::Arc;

async fn setup() -> (TestApp, String) {
    let url = std::env::var("LOCAL_CALENDAR_TEST_DATABASE_URL")
        .expect("LOCAL_CALENDAR_TEST_DATABASE_URL must point at the isolated calendar test DB");
    let database = url
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .split('?')
        .next()
        .unwrap_or_default();
    assert_eq!(database, "outreach_local_booking_backend");
    let owner = Db::connect(&url, None).await.expect("connect owner");
    owner.migrate().await.expect("apply migrations");
    let db = Db::connect(&url, Some(ROLE_API))
        .await
        .expect("connect api role");
    let world = seed_workspace(&db).await.unwrap();
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
    let csrf = TestApp::csrf_token_from(&login).expect("login response sets CSRF cookie");
    (app, csrf)
}

async fn connect(app: &TestApp, csrf: &str, key: &str) -> reqwest::Response {
    app.client
        .post(app.url("/api/v1/calendar-connections/local"))
        .header("x-csrf-token", csrf)
        .header("Idempotency-Key", key)
        .json(&json!({}))
        .send()
        .await
        .unwrap()
}

fn data(response: &Value) -> &Value {
    response.get("data").expect("Envelope data")
}

#[tokio::test]
async fn local_calendar_connection_is_idempotent_and_one_per_host() {
    let (app, csrf) = setup().await;
    let first = connect(&app, &csrf, "local-calendar-first").await;
    assert_eq!(first.status(), StatusCode::CREATED);
    let first_body: Value = first.json().await.unwrap();
    let first_data = data(&first_body);
    assert_eq!(first_data["provider"], "office_calendar");
    assert_eq!(first_data["status"], "active");
    let first_id = first_data["id"].as_str().unwrap().to_owned();
    let local_calendar_id = first_data["provider_calendar_id"].as_str().unwrap();
    assert!(local_calendar_id.starts_with("office:"));

    let replay = connect(&app, &csrf, "local-calendar-first").await;
    assert_eq!(replay.status(), StatusCode::CREATED);
    let replay_body: Value = replay.json().await.unwrap();
    assert_eq!(data(&replay_body)["id"], first_id);

    let existing = connect(&app, &csrf, "local-calendar-second").await;
    assert_eq!(existing.status(), StatusCode::OK);
    let existing_body: Value = existing.json().await.unwrap();
    assert_eq!(data(&existing_body)["id"], first_id);

    let no_csrf = app
        .client
        .post(app.url("/api/v1/calendar-connections/local"))
        .header("Idempotency-Key", "local-calendar-no-csrf")
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(no_csrf.status(), StatusCode::FORBIDDEN);
}
