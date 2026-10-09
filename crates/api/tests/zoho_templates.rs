//! API coverage for the separately configured prepaid Zoho Email API v2.
//! All provider behavior is exercised against an ephemeral loopback server.

use api::{AppState, testing::TestApp};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use infrastructure::{
    config::Config,
    db::{Db, ROLE_API},
    providers::{zoho_campaigns::SecretValue, zoho_email_api::ZohoEmailApi},
    testing::{TEST_PASSWORD, TestWorld, seed_workspace},
};
use reqwest::StatusCode as ClientStatus;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::sync::Mutex;

#[derive(Clone)]
struct ProviderState {
    writes: Arc<AtomicUsize>,
    fail_create_unknown: Arc<AtomicBool>,
    rate_limit_next_create: Arc<AtomicBool>,
    details: Arc<Mutex<Value>>,
    list_ranges: Arc<Mutex<Vec<(u32, u32)>>>,
}

fn provider_router(state: ProviderState) -> Router {
    Router::new()
        .route(
            "/emailapi/v2/templates",
            get(list_templates).post(create_template),
        )
        .route(
            "/emailapi/v2/templates/{id}",
            get(get_template)
                .put(update_template)
                .delete(delete_template),
        )
        .with_state(state)
}

#[derive(Deserialize)]
struct ListRange {
    start_index: u32,
    end_index: u32,
}

async fn list_templates(
    State(state): State<ProviderState>,
    Query(range): Query<ListRange>,
) -> Json<Value> {
    state
        .list_ranges
        .lock()
        .await
        .push((range.start_index, range.end_index));
    Json(json!({"response":{"code": 200701}, "templates": [{"id": "tpl-1", "name": "welcome"}]}))
}

async fn get_template(State(state): State<ProviderState>) -> Json<Value> {
    Json(
        json!({"response":{"code": 200702}, "template_details": state.details.lock().await.clone()}),
    )
}

async fn create_template(State(state): State<ProviderState>, Json(body): Json<Value>) -> Response {
    state.writes.fetch_add(1, Ordering::SeqCst);
    let details = json!({
        "id": "tpl-1",
        "name": body["template_name"],
        "content_type": body["content_type"],
        "content": body["content"],
    });
    if state.fail_create_unknown.swap(false, Ordering::SeqCst) {
        *state.details.lock().await = details;
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"response":{"code": 500001}})),
        )
            .into_response();
    }
    if state.rate_limit_next_create.swap(false, Ordering::SeqCst) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(header::RETRY_AFTER, "2")],
            Json(json!({"response":{"code": 429001}})),
        )
            .into_response();
    }
    *state.details.lock().await = details;
    (
        StatusCode::OK,
        Json(json!({"response":{"code": 200703}, "template_id": "tpl-1"})),
    )
        .into_response()
}

async fn update_template(
    State(state): State<ProviderState>,
    Json(body): Json<Value>,
) -> Json<Value> {
    state.writes.fetch_add(1, Ordering::SeqCst);
    *state.details.lock().await = json!({
        "id": "tpl-1",
        "name": body["template_name"],
        "content_type": body["content_type"],
        "content": body["content"],
    });
    Json(json!({"response":{"code": 200704}}))
}

async fn delete_template(State(state): State<ProviderState>) -> Json<Value> {
    state.writes.fetch_add(1, Ordering::SeqCst);
    Json(json!({"response":{"code": 200705}}))
}

async fn setup() -> Option<(
    TestApp,
    TestWorld,
    ProviderState,
    tokio::task::JoinHandle<()>,
)> {
    setup_with_email_api(true).await
}

async fn setup_with_email_api(
    enabled: bool,
) -> Option<(
    TestApp,
    TestWorld,
    ProviderState,
    tokio::task::JoinHandle<()>,
)> {
    let url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping API test");
            return None;
        }
    };
    let owner = Db::connect(&url, None).await.unwrap();
    owner.migrate().await.unwrap();
    let db = Db::connect(&url, Some(ROLE_API)).await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let provider_state = ProviderState {
        writes: Arc::new(AtomicUsize::new(0)),
        fail_create_unknown: Arc::new(AtomicBool::new(false)),
        rate_limit_next_create: Arc::new(AtomicBool::new(false)),
        list_ranges: Arc::new(Mutex::new(Vec::new())),
        details: Arc::new(Mutex::new(json!({
            "id": "tpl-1", "name": "welcome", "content_type": "html", "content": "<p>initial</p>"
        }))),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let mock_router = provider_router(provider_state.clone());
    let handle = tokio::spawn(async move { axum::serve(listener, mock_router).await.unwrap() });

    let mut config = Config::test();
    config.zoho.fake_for_tests = false;
    config.zoho.email_api.enabled = enabled;
    config.zoho.email_api.api_key = Some(SecretValue::new("private-zoho-api-key"));
    config.zoho.email_api.workspace_id = Some(world.workspace_id);
    let mut state = AppState::with_fake_providers(
        db.with_default_workspace(Some(world.workspace_id)),
        Arc::new(config),
        None,
    );
    state.email_api = Some(Arc::new(
        ZohoEmailApi::for_test(Some(SecretValue::new("private-zoho-api-key")), &base).unwrap(),
    ));
    let app = TestApp::spawn_with_state(state).await;
    Some((app, world, provider_state, handle))
}

async fn login(app: &TestApp, world: &TestWorld) -> String {
    let response = app
        .login(world.admin.email.as_deref().unwrap(), TEST_PASSWORD)
        .await;
    assert_eq!(response.status(), ClientStatus::OK);
    TestApp::csrf_token_from(&response).unwrap()
}

fn write_body(name: &str, content: &str) -> Value {
    json!({"name": name, "content": content, "content_type": "html"})
}

async fn create(app: &TestApp, csrf: &str, key: &str, body: Value) -> reqwest::Response {
    app.client
        .post(app.url("/api/v1/zoho-email-api/templates"))
        .header("x-csrf-token", csrf)
        .header("idempotency-key", key)
        .json(&body)
        .send()
        .await
        .unwrap()
}

async fn update(
    app: &TestApp,
    csrf: &str,
    key: &str,
    etag: Option<&str>,
    body: Value,
) -> reqwest::Response {
    let mut request = app
        .client
        .put(app.url("/api/v1/zoho-email-api/templates/tpl-1"))
        .header("x-csrf-token", csrf)
        .header("idempotency-key", key)
        .json(&body);
    if let Some(etag) = etag {
        request = request.header("if-match", etag);
    }
    request.send().await.unwrap()
}

async fn delete(app: &TestApp, csrf: &str, key: &str, etag: Option<&str>) -> reqwest::Response {
    let mut request = app
        .client
        .delete(app.url("/api/v1/zoho-email-api/templates/tpl-1"))
        .header("x-csrf-token", csrf)
        .header("idempotency-key", key);
    if let Some(etag) = etag {
        request = request.header("if-match", etag);
    }
    request.send().await.unwrap()
}

#[tokio::test]
async fn template_mutations_are_cached_metadata_only_and_use_etag_preconditions() {
    let Some((app, world, provider, handle)) = setup().await else {
        return;
    };
    let csrf = login(&app, &world).await;
    let secret_html =
        "<html><body><a href=\"https://private.invalid/book/one\">Book</a></body></html>";

    let response = create(
        &app,
        &csrf,
        "create-once",
        write_body("welcome", secret_html),
    )
    .await;
    assert_eq!(response.status(), ClientStatus::CREATED);
    let created = TestApp::data(response).await;
    assert_eq!(created["template_id"], "tpl-1");
    assert_eq!(created["deleted"], false);
    assert!(created.get("content").is_none());
    assert!(!created.to_string().contains(secret_html));
    assert!(!created.to_string().contains("private-zoho-api-key"));
    let operation_id = created["operation_id"].as_str().unwrap().to_string();

    // A changed server credential blocks replay of the old key before Zoho
    // is contacted again.
    let mut rotated_config = (*app.state.config).clone();
    rotated_config.zoho.email_api.api_key = Some(SecretValue::new("rotated-zoho-api-key"));
    let mut rotated_state =
        AppState::with_fake_providers(app.state.db.clone(), Arc::new(rotated_config), None);
    rotated_state.email_api = app.state.email_api.clone();
    let rotated_app = TestApp::spawn_with_state(rotated_state).await;
    let rotated_csrf = login(&rotated_app, &world).await;
    let rotated_replay = create(
        &rotated_app,
        &rotated_csrf,
        "create-once",
        write_body("welcome", secret_html),
    )
    .await;
    let problem = TestApp::problem(rotated_replay, ClientStatus::CONFLICT).await;
    assert_eq!(problem["code"], "CONFLICT");
    assert_eq!(provider.writes.load(Ordering::SeqCst), 1);

    let replay = create(
        &app,
        &csrf,
        "create-once",
        write_body("welcome", secret_html),
    )
    .await;
    assert_eq!(replay.status(), ClientStatus::CREATED);
    assert_eq!(TestApp::data(replay).await["operation_id"], operation_id);
    assert_eq!(
        provider.writes.load(Ordering::SeqCst),
        1,
        "completed create must not be repeated"
    );

    let detail = app.get("/api/v1/zoho-email-api/templates/tpl-1").await;
    assert_eq!(detail.status(), ClientStatus::OK);
    let etag = detail
        .headers()
        .get("etag")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let detail = TestApp::data(detail).await;
    assert_eq!(detail["content"], secret_html);
    assert_eq!(detail["content_hash"], etag.trim_matches('"'));

    let updated_html = "<html><body>updated booking</body></html>";
    let response = update(
        &app,
        &csrf,
        "update-once",
        Some(&etag),
        write_body("welcome", updated_html),
    )
    .await;
    assert_eq!(response.status(), ClientStatus::OK);
    let updated = TestApp::data(response).await;
    assert_eq!(updated["template_id"], "tpl-1");
    assert_eq!(provider.writes.load(Ordering::SeqCst), 2);
    let update_replay = update(
        &app,
        &csrf,
        "update-once",
        Some(&etag),
        write_body("welcome", updated_html),
    )
    .await;
    assert_eq!(update_replay.status(), ClientStatus::OK);
    assert_eq!(
        TestApp::data(update_replay).await["operation_id"],
        updated["operation_id"]
    );
    assert_eq!(provider.writes.load(Ordering::SeqCst), 2);
    let body_mismatch = update(
        &app,
        &csrf,
        "update-once",
        Some(&etag),
        write_body("welcome", "different html"),
    )
    .await;
    let mismatch = TestApp::problem(body_mismatch, ClientStatus::CONFLICT).await;
    assert_eq!(mismatch["code"], "IDEMPOTENCY_KEY_REUSED");
    assert_eq!(
        provider.writes.load(Ordering::SeqCst),
        2,
        "changed body under same key must not call Zoho"
    );

    let updated_detail = app.get("/api/v1/zoho-email-api/templates/tpl-1").await;
    let updated_etag = updated_detail
        .headers()
        .get("etag")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(TestApp::data(updated_detail).await["content"], updated_html);
    let response = delete(&app, &csrf, "delete-once", Some(&updated_etag)).await;
    assert_eq!(response.status(), ClientStatus::OK);
    let deleted = TestApp::data(response).await;
    assert_eq!(deleted["deleted"], true);
    assert_eq!(deleted["template_id"], "tpl-1");
    let replay = delete(&app, &csrf, "delete-once", Some(&updated_etag)).await;
    assert_eq!(
        TestApp::data(replay).await["operation_id"],
        deleted["operation_id"]
    );
    assert_eq!(
        provider.writes.load(Ordering::SeqCst),
        3,
        "delete replay must use the cached result"
    );

    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let stored: String = sqlx::query_scalar("SELECT COALESCE(jsonb_agg(to_jsonb(op))::text, '') FROM zoho_email_template_operations op WHERE workspace_id=$1")
        .bind(world.workspace_id).fetch_one(tx.conn()).await.unwrap();
    let audit: String = sqlx::query_scalar("SELECT COALESCE(string_agg(details::text, ''), '') FROM audit_logs WHERE workspace_id=$1 AND action LIKE 'marketing.zoho_email_api.%'")
        .bind(world.workspace_id).fetch_one(tx.conn()).await.unwrap();
    assert!(!stored.contains("private-zoho-api-key"));
    assert!(!stored.contains("private.invalid"));
    assert!(!stored.contains(secret_html));
    assert!(!stored.contains(updated_html));
    assert!(!stored.contains("different html"));
    assert!(!audit.contains("private-zoho-api-key"));
    assert!(!audit.contains("private.invalid"));
    assert!(!audit.contains(secret_html));
    assert!(!audit.contains(updated_html));
    assert!(!audit.contains("different html"));
    tx.commit().await.unwrap();
    handle.abort();
}

#[tokio::test]
async fn list_uses_a_principal_bound_cursor_and_email_api_limit_of_one_hundred() {
    let Some((app, world, provider, handle)) = setup().await else {
        return;
    };
    let _csrf = login(&app, &world).await;
    let first = app
        .client
        .get(app.url("/api/v1/zoho-email-api/templates"))
        .query(&[("limit", "1")])
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), ClientStatus::OK);
    let first: Value = first.json().await.unwrap();
    let cursor = first["page"]["next_cursor"].as_str().unwrap().to_string();
    assert_eq!(provider.list_ranges.lock().await.as_slice(), &[(1, 1)]);

    let second = app
        .client
        .get(app.url("/api/v1/zoho-email-api/templates"))
        .query(&[("limit", "1"), ("cursor", cursor.as_str())])
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), ClientStatus::OK);
    assert_eq!(
        provider.list_ranges.lock().await.as_slice(),
        &[(1, 1), (2, 2)]
    );

    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    sqlx::query("UPDATE principals SET role='admin',permission_version=permission_version+1 WHERE workspace_id=$1 AND id=$2")
        .bind(world.workspace_id)
        .bind(world.member_a.principal_id)
        .execute(tx.conn())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let other_principal_app = TestApp::spawn_with_state(app.state.clone()).await;
    let member = other_principal_app
        .login(world.member_a.email.as_deref().unwrap(), TEST_PASSWORD)
        .await;
    assert_eq!(member.status(), ClientStatus::OK);
    let wrong_principal = other_principal_app
        .client
        .get(other_principal_app.url("/api/v1/zoho-email-api/templates"))
        .query(&[("limit", "1"), ("cursor", cursor.as_str())])
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_principal.status(), ClientStatus::BAD_REQUEST);
    assert_eq!(
        provider.list_ranges.lock().await.as_slice(),
        &[(1, 1), (2, 2)]
    );

    let (body, signature) = cursor.split_once('.').unwrap();
    let first_sig = signature.chars().next().unwrap();
    let replacement = if first_sig == 'a' { 'b' } else { 'a' };
    let tampered = format!("{body}.{replacement}{}", &signature[1..]);
    let bad_cursor = app
        .client
        .get(app.url("/api/v1/zoho-email-api/templates"))
        .query(&[("limit", "1"), ("cursor", tampered.as_str())])
        .send()
        .await
        .unwrap();
    assert_eq!(bad_cursor.status(), ClientStatus::BAD_REQUEST);
    let too_large = app
        .client
        .get(app.url("/api/v1/zoho-email-api/templates"))
        .query(&[("limit", "101")])
        .send()
        .await
        .unwrap();
    assert_eq!(too_large.status(), ClientStatus::BAD_REQUEST);
    assert_eq!(
        provider.list_ranges.lock().await.as_slice(),
        &[(1, 1), (2, 2)]
    );
    handle.abort();
}

#[tokio::test]
async fn stale_or_missing_if_match_never_mutates_and_unknown_create_is_not_replayed() {
    let Some((app, world, provider, handle)) = setup().await else {
        return;
    };
    let csrf = login(&app, &world).await;
    let stale_etag = format!("\"{}\"", "0".repeat(64));
    let no_match = update(
        &app,
        &csrf,
        "missing-if-match",
        None,
        write_body("welcome", "changed"),
    )
    .await;
    let problem = TestApp::problem(no_match, ClientStatus::from_u16(428).unwrap()).await;
    assert_eq!(problem["code"], "VERSION_PRECONDITION_REQUIRED");
    assert_eq!(provider.writes.load(Ordering::SeqCst), 0);

    let stale = update(
        &app,
        &csrf,
        "stale-if-match",
        Some(&stale_etag),
        write_body("welcome", "changed"),
    )
    .await;
    let problem = TestApp::problem(stale, ClientStatus::PRECONDITION_FAILED).await;
    assert_eq!(problem["code"], "VERSION_MISMATCH");
    assert_eq!(provider.writes.load(Ordering::SeqCst), 0);

    let delete_missing = delete(&app, &csrf, "delete-missing-if-match", None).await;
    let problem = TestApp::problem(delete_missing, ClientStatus::from_u16(428).unwrap()).await;
    assert_eq!(problem["code"], "VERSION_PRECONDITION_REQUIRED");
    let delete_stale = delete(&app, &csrf, "delete-stale-if-match", Some(&stale_etag)).await;
    let problem = TestApp::problem(delete_stale, ClientStatus::PRECONDITION_FAILED).await;
    assert_eq!(problem["code"], "VERSION_MISMATCH");
    assert_eq!(provider.writes.load(Ordering::SeqCst), 0);

    provider.fail_create_unknown.store(true, Ordering::SeqCst);
    let response = create(
        &app,
        &csrf,
        "ambiguous-create",
        write_body("welcome", "possibly-created"),
    )
    .await;
    let problem = TestApp::problem(response, ClientStatus::CONFLICT).await;
    assert_eq!(problem["code"], "PROVIDER_SUBMISSION_OUTCOME_UNKNOWN");
    let retry = create(
        &app,
        &csrf,
        "ambiguous-create",
        write_body("welcome", "possibly-created"),
    )
    .await;
    let problem = TestApp::problem(retry, ClientStatus::CONFLICT).await;
    assert_eq!(problem["code"], "PROVIDER_SUBMISSION_OUTCOME_UNKNOWN");
    assert_eq!(
        provider.writes.load(Ordering::SeqCst),
        1,
        "unknown create must not be sent a second time"
    );
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let state: String = sqlx::query_scalar(
        "SELECT state FROM zoho_email_template_operations WHERE workspace_id=$1 AND actor_principal_id=$2 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(world.workspace_id)
    .bind(world.admin.principal_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(state, "unknown");
    let stored: String = sqlx::query_scalar(
        "SELECT COALESCE(jsonb_agg(to_jsonb(op))::text, '') FROM zoho_email_template_operations op WHERE workspace_id=$1 AND actor_principal_id=$2",
    )
    .bind(world.workspace_id)
    .bind(world.admin.principal_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let audit: String = sqlx::query_scalar(
        "SELECT COALESCE(string_agg(details::text, ''), '') FROM audit_logs WHERE workspace_id=$1 AND action='marketing.zoho_email_api.template_create'",
    )
    .bind(world.workspace_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert!(!stored.contains("possibly-created"));
    assert!(!audit.contains("possibly-created"));
    tx.commit().await.unwrap();
    handle.abort();
}

#[tokio::test]
async fn agent_and_non_owner_workspace_cannot_use_oracle_key() {
    let Some((app, world, provider, handle)) = setup().await else {
        return;
    };
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let (_, token) = infrastructure::repo::auth::create_service_credential(
        &mut tx,
        world.agent.principal_id,
        "Zoho template denial test",
        &["operations.read".into()],
        chrono::Duration::hours(1),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let agent = reqwest::Client::new()
        .post(app.url("/api/v1/zoho-email-api/templates"))
        .bearer_auth(token)
        .header("idempotency-key", "agent-cannot-use")
        .json(&write_body("welcome", "must-not-send"))
        .send()
        .await
        .unwrap();
    let problem = TestApp::problem(agent, ClientStatus::FORBIDDEN).await;
    assert_eq!(problem["code"], "FORBIDDEN");
    assert_eq!(provider.writes.load(Ordering::SeqCst), 0);
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let denied: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE workspace_id=$1 AND action='marketing.zoho_email_api.template_create' AND details->>'outcome'='denied:FORBIDDEN'")
        .bind(world.workspace_id).fetch_one(tx.conn()).await.unwrap();
    assert_eq!(denied, 1);
    tx.commit().await.unwrap();

    let foreign_world = seed_workspace(&app.state.db).await.unwrap();
    let config = (*app.state.config).clone();
    assert_eq!(config.zoho.email_api.workspace_id, Some(world.workspace_id));
    let foreign_db = app
        .state
        .db
        .clone()
        .with_default_workspace(Some(foreign_world.workspace_id));
    let mut foreign_state = AppState::with_fake_providers(foreign_db, Arc::new(config), None);
    foreign_state.email_api = app.state.email_api.clone();
    let foreign_app = TestApp::spawn_with_state(foreign_state).await;
    let foreign_csrf = login(&foreign_app, &foreign_world).await;
    let response = create(
        &foreign_app,
        &foreign_csrf,
        "foreign-workspace",
        write_body("welcome", "must-not-send"),
    )
    .await;
    let problem = TestApp::problem(response, ClientStatus::FORBIDDEN).await;
    assert_eq!(problem["code"], "FORBIDDEN");
    assert_eq!(provider.writes.load(Ordering::SeqCst), 0);
    handle.abort();
}

#[tokio::test]
async fn disabled_email_api_flag_blocks_remote_calls_even_when_client_exists() {
    let Some((app, world, provider, handle)) = setup_with_email_api(false).await else {
        return;
    };
    let csrf = login(&app, &world).await;
    let response = create(
        &app,
        &csrf,
        "disabled-email-api",
        write_body("welcome", "must-not-send"),
    )
    .await;
    let problem = TestApp::problem(response, ClientStatus::SERVICE_UNAVAILABLE).await;
    assert_eq!(problem["code"], "DEPENDENCY_UNAVAILABLE");
    assert_eq!(provider.writes.load(Ordering::SeqCst), 0);
    handle.abort();
}

#[tokio::test]
async fn missing_email_api_key_blocks_remote_calls_even_when_client_exists() {
    let Some((app, world, provider, handle)) = setup().await else {
        return;
    };
    let mut config = (*app.state.config).clone();
    config.zoho.email_api.api_key = None;
    let mut state = AppState::with_fake_providers(app.state.db.clone(), Arc::new(config), None);
    state.email_api = app.state.email_api.clone();
    let unconfigured_app = TestApp::spawn_with_state(state).await;
    let csrf = login(&unconfigured_app, &world).await;
    let response = create(
        &unconfigured_app,
        &csrf,
        "missing-email-api-key",
        write_body("welcome", "must-not-send"),
    )
    .await;
    let problem = TestApp::problem(response, ClientStatus::SERVICE_UNAVAILABLE).await;
    assert_eq!(problem["code"], "DEPENDENCY_UNAVAILABLE");
    assert_eq!(provider.writes.load(Ordering::SeqCst), 0);
    handle.abort();
}

#[tokio::test]
async fn definitive_rate_limit_releases_claim_and_maps_retry_after() {
    let Some((app, world, provider, handle)) = setup().await else {
        return;
    };
    let csrf = login(&app, &world).await;
    provider
        .rate_limit_next_create
        .store(true, Ordering::SeqCst);
    let limited = create(
        &app,
        &csrf,
        "retry-after-limit",
        write_body("welcome", "rate limited once"),
    )
    .await;
    assert_eq!(limited.headers().get("retry-after").unwrap(), "2");
    let problem = TestApp::problem(limited, ClientStatus::TOO_MANY_REQUESTS).await;
    assert_eq!(problem["code"], "RATE_LIMITED");
    assert_eq!(problem["retryable"], true);

    let retry = create(
        &app,
        &csrf,
        "retry-after-limit",
        write_body("welcome", "rate limited once"),
    )
    .await;
    assert_eq!(retry.status(), ClientStatus::CREATED);
    assert_eq!(provider.writes.load(Ordering::SeqCst), 2);
    handle.abort();
}

#[tokio::test]
async fn template_updates_respect_database_locks_from_other_api_processes() {
    let Some((app, world, provider_state, handle)) = setup().await else {
        return;
    };
    let csrf = login(&app, &world).await;
    let detail = app
        .client
        .get(app.url("/api/v1/zoho-email-api/templates/tpl-1"))
        .send()
        .await
        .unwrap();
    let etag = detail
        .headers()
        .get("etag")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let digest = infrastructure::crypto::sha256_hex(
        format!("zoho-template-lock:{}:tpl-1", world.workspace_id).as_bytes(),
    );
    let key = u64::from_str_radix(&digest[..16], 16).unwrap() as i64;
    // Simulate a writer in another API process using its own database session.
    let mut other_writer = app.state.db.begin(world.workspace_id).await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(key)
        .execute(other_writer.conn())
        .await
        .unwrap();
    let mut request = Box::pin(update(
        &app,
        &csrf,
        "cross-process-update",
        Some(&etag),
        write_body("updated", "<p>updated</p>"),
    ));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(150), &mut request)
            .await
            .is_err()
    );
    assert_eq!(provider_state.writes.load(Ordering::SeqCst), 0);
    other_writer.rollback().await.unwrap();
    assert_eq!(request.await.status(), ClientStatus::OK);
    assert_eq!(provider_state.writes.load(Ordering::SeqCst), 1);
    handle.abort();
}

#[tokio::test]
async fn ten_concurrent_template_writers_leave_capacity_for_durable_journals() {
    let Some((app, world, _, handle)) = setup().await else {
        return;
    };
    let csrf = login(&app, &world).await;
    let detail = app
        .client
        .get(app.url("/api/v1/zoho-email-api/templates/tpl-1"))
        .send()
        .await
        .unwrap();
    let etag = detail
        .headers()
        .get("etag")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let mut requests = tokio::task::JoinSet::new();
    for index in 0..10 {
        let client = app.client.clone();
        let url = app.url("/api/v1/zoho-email-api/templates/tpl-1");
        let csrf = csrf.clone();
        let etag = etag.clone();
        requests.spawn(async move {
            client
                .put(url)
                .header("x-csrf-token", csrf)
                .header("idempotency-key", format!("concurrent-write-{index}"))
                .header("if-match", etag)
                .json(&write_body(&format!("updated-{index}"), "<p>updated</p>"))
                .send()
                .await
                .unwrap()
                .status()
        });
    }
    let results = tokio::time::timeout(std::time::Duration::from_secs(4), async {
        let mut successes = 0;
        while let Some(result) = requests.join_next().await {
            match result.unwrap() {
                ClientStatus::OK => successes += 1,
                ClientStatus::PRECONDITION_FAILED => {}
                other => panic!("unexpected concurrent-write status: {other}"),
            }
        }
        successes
    })
    .await
    .expect("writers must not exhaust the journal connection pool");
    assert_eq!(results, 1);
    handle.abort();
}
