//! Integrated Office-to-Zoho Campaigns Free delivery tests.
//!
//! Office content, campaign launches, approvals, exports, and delivery run
//! through the real API/application/database. Zoho is represented by a
//! stateful loopback server; no live credentials or provider requests are used.

use api::{AppState, testing::TestApp};
use axum::{
    Router,
    body::{Body, Bytes},
    extract::{OriginalUri, State},
    http::{HeaderMap, Method, StatusCode},
    response::Response,
    routing::any,
};
use chrono::{Duration, Utc};
use infrastructure::{
    config::Config,
    db::{Db, ROLE_API},
    providers::zoho_campaigns::{
        CAMPAIGNS_SCOPES, CampaignsOAuthConfig, SecretValue,
        http::{CampaignsHttp, OAuthTokens},
        operations::CampaignsOperations,
    },
    repo::{campaigns, marketing, marketing_exports},
    secrets::{FileSecretStore, SecretStore},
    testing::{TEST_PASSWORD, TestWorld, seed_workspace},
};
use reqwest::Url;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use tokio::sync::{Mutex, Notify};
use uuid::Uuid;

const CONTENT_FIELD: &str = "Booking URL";
const OFFICE_UNSUBSCRIBE_FIELD: &str = "Office Unsubscribe URL";
const OFFICE_UNSUBSCRIBE_TAG: &str = "$[UD:OFFICE_UNSUBSCRIBE_URL]$";
const CONTENT_TAG: &str = "$[UD:BOOKING_URL]$";

#[derive(Debug, Clone)]
struct CapturedRequest {
    method: Method,
    path: String,
    params: BTreeMap<String, String>,
    authorization: Option<String>,
}

#[derive(Debug, Clone)]
struct MockCampaign {
    name: String,
    subject: String,
    from_email: String,
    from_name: String,
    list_key: String,
    status: String,
}

type MockListStore = HashMap<String, BTreeMap<String, BTreeMap<String, String>>>;

#[derive(Clone, Default)]
struct ZohoMock {
    omit_custom_fields: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
    lists: Arc<Mutex<MockListStore>>,
    unsubscribed: Arc<Mutex<HashMap<String, HashSet<String>>>>,
    campaigns: Arc<Mutex<HashMap<String, MockCampaign>>>,
    ambiguous_campaigns: Arc<Mutex<HashSet<String>>>,
    blocked_campaigns: Arc<Mutex<HashSet<String>>>,
    send_received: Arc<Notify>,
    release_send: Arc<Notify>,
    next_list: Arc<AtomicUsize>,
    next_campaign: Arc<AtomicUsize>,
    send_calls: Arc<AtomicUsize>,
}

fn decode_form(raw: &[u8]) -> BTreeMap<String, String> {
    let url = Url::parse(&format!(
        "http://mock.invalid/?{}",
        String::from_utf8_lossy(raw)
    ))
    .unwrap();
    url.query_pairs().into_owned().collect()
}

fn json_reply(status: StatusCode, value: Value) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(value.to_string()))
        .unwrap()
}

async fn zoho_request(
    State(mock): State<ZohoMock>,
    OriginalUri(uri): OriginalUri,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response<Body> {
    let query = uri
        .query()
        .map(|query| decode_form(query.as_bytes()))
        .unwrap_or_default();
    let mut params = query;
    if !body.is_empty() {
        params.extend(decode_form(&body));
    }
    let path = uri.path().to_owned();
    let authorization = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    mock.requests.lock().await.push(CapturedRequest {
        method: method.clone(),
        path: path.clone(),
        params: params.clone(),
        authorization,
    });

    if path == "/oauth/v2/token" {
        return json_reply(
            StatusCode::OK,
            json!({
                "access_token": "loopback-access-token",
                "refresh_token": "loopback-refresh-token",
                "expires_in": 3600
            }),
        );
    }

    let endpoint = path.strip_prefix("/api/v1.1/").unwrap_or(path.as_str());
    match endpoint {
        "contact/allfields" => json_reply(
            StatusCode::OK,
            json!({"code":"0","fieldname":[{
                "DISPLAY_NAME":CONTENT_FIELD,
                "FIELD_NAME":"BOOKING_URL",
                "FIELD_ID":"9001"
            },{
                "DISPLAY_NAME":OFFICE_UNSUBSCRIBE_FIELD,
                "FIELD_NAME":"OFFICE_UNSUBSCRIBE_URL",
                "FIELD_ID":"9002"
            }]}),
        ),
        "topics" => json_reply(StatusCode::OK, json!({"code":"200","topicDetails":[]})),
        "addlistandcontacts" => {
            let index = mock.next_list.fetch_add(1, Ordering::SeqCst) + 1;
            let list_key = format!("office-list-{index}");
            let contacts = params
                .get("emailids")
                .into_iter()
                .flat_map(|emails| emails.split(','))
                .map(|email| (email.to_ascii_lowercase(), BTreeMap::new()))
                .collect();
            mock.lists.lock().await.insert(list_key.clone(), contacts);
            json_reply(
                StatusCode::OK,
                json!({"code":"0","listkey":list_key,"listname":params.get("listname")}),
            )
        }
        "json/listsubscribe" => {
            let list_key = params.get("listkey").cloned().unwrap_or_default();
            let contact: Value = params
                .get("contactinfo")
                .and_then(|raw| serde_json::from_str(raw).ok())
                .unwrap_or(Value::Null);
            let email = contact["Contact Email"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            let fields = contact
                .as_object()
                .into_iter()
                .flat_map(|object| object.iter())
                .filter(|(label, _)| label.as_str() != "Contact Email")
                .map(|(label, value)| {
                    (label.clone(), value.as_str().unwrap_or_default().to_owned())
                })
                .collect();
            if let Some(list) = mock.lists.lock().await.get_mut(&list_key) {
                list.insert(email.to_ascii_lowercase(), fields);
            }
            json_reply(StatusCode::OK, json!({"code":"0","message":"success"}))
        }
        "getlistsubscribers" => {
            let list_key = params.get("listkey").cloned().unwrap_or_default();
            let status = params.get("status").map(String::as_str).unwrap_or_default();
            let contacts = mock
                .lists
                .lock()
                .await
                .get(&list_key)
                .cloned()
                .unwrap_or_default();
            let unsubscribed = mock
                .unsubscribed
                .lock()
                .await
                .get(&list_key)
                .cloned()
                .unwrap_or_default();
            let rows = contacts
                .into_iter()
                .filter(|(email, _)| match status {
                    "active" => !unsubscribed.contains(email),
                    "unsub" => unsubscribed.contains(email),
                    _ => false,
                })
                .map(|(email, fields)| {
                    let mut row = serde_json::Map::new();
                    row.insert("contact_email".into(), Value::String(email));
                    row.extend(
                        fields
                            .into_iter()
                            .filter(|_| !mock.omit_custom_fields.load(Ordering::SeqCst))
                            .map(|(label, value)| {
                                let api_name = if label == CONTENT_FIELD {
                                    "BOOKING_URL".to_owned()
                                } else if label == OFFICE_UNSUBSCRIBE_FIELD {
                                    "OFFICE_UNSUBSCRIBE_URL".to_owned()
                                } else {
                                    label
                                };
                                (api_name, Value::String(value))
                            }),
                    );
                    Value::Object(row)
                })
                .collect::<Vec<_>>();
            json_reply(
                StatusCode::OK,
                json!({
                    "code":"0",
                    "requestdetails":{"fromindex":1,"range":100},
                    "list_of_details":rows
                }),
            )
        }
        "createCampaign" => {
            let index = mock.next_campaign.fetch_add(1, Ordering::SeqCst) + 1;
            let campaign_key = format!("office-campaign-{index}");
            let list_details: Value = params
                .get("list_details")
                .and_then(|raw| serde_json::from_str(raw).ok())
                .unwrap_or(Value::Null);
            let list_key = list_details
                .as_object()
                .and_then(|object| object.keys().next())
                .cloned()
                .unwrap_or_default();
            mock.campaigns.lock().await.insert(
                campaign_key.clone(),
                MockCampaign {
                    name: params.get("campaignname").cloned().unwrap_or_default(),
                    subject: params.get("subject").cloned().unwrap_or_default(),
                    from_email: params.get("from_email").cloned().unwrap_or_default(),
                    from_name: params.get("from_name").cloned().unwrap_or_default(),
                    list_key,
                    status: "Draft".into(),
                },
            );
            json_reply(
                StatusCode::OK,
                json!({"code":"200","campaignKey":campaign_key}),
            )
        }
        "getcampaigndetails" => {
            let key = params.get("campaignkey").cloned().unwrap_or_default();
            let campaigns = mock.campaigns.lock().await;
            let Some(campaign) = campaigns.get(&key) else {
                return json_reply(StatusCode::OK, json!({"code":"6101"}));
            };
            json_reply(
                StatusCode::OK,
                json!({
                    "code":"0",
                    "campaign_status":campaign.status,
                    "campaign-details":[{
                        "campaign_key":key,
                        "campaign_name":campaign.name,
                        "email_subject":campaign.subject,
                        "email_from":campaign.from_email,
                        "sender_name":campaign.from_name
                    }],
                    "associated_mailing_lists":[{"listkey":campaign.list_key}]
                }),
            )
        }
        "campaignreports" => json_reply(
            StatusCode::OK,
            json!({
                "code":"0",
                "campaign-reports":[{"emails_sent_count":2,"opens_count":1}],
                "campaign-details":[{"campaign_key":params.get("campaignkey")}],
                "campaign-reach":[{"total":2}]
            }),
        ),
        "getcampaignrecipientsdata" => json_reply(
            StatusCode::OK,
            json!({
                "code":"0",
                "requestdetails":{"fromindex":1,"range":0},
                "list_of_details":[]
            }),
        ),
        "sendcampaign" => {
            mock.send_calls.fetch_add(1, Ordering::SeqCst);
            let key = params.get("campaignkey").cloned().unwrap_or_default();
            if mock.blocked_campaigns.lock().await.contains(&key) {
                mock.send_received.notify_one();
                mock.release_send.notified().await;
            }
            let ambiguous = mock.ambiguous_campaigns.lock().await.contains(&key);
            if let Some(campaign) = mock.campaigns.lock().await.get_mut(&key) {
                // The provider accepted the one bulk send even when its HTTP
                // response below is deliberately ambiguous.
                campaign.status = "inprogress".into();
            }
            if ambiguous {
                json_reply(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    json!({"code":"500","message":"accepted; response lost"}),
                )
            } else {
                json_reply(
                    StatusCode::OK,
                    json!({"code":"200","campaign_status":"inprogress"}),
                )
            }
        }
        _ => json_reply(StatusCode::NOT_FOUND, json!({"code":"404"})),
    }
}

struct Fixture {
    app: TestApp,
    world: TestWorld,
    admin: reqwest::Client,
    admin_csrf: String,
    manager: reqwest::Client,
    manager_csrf: String,
    channel_id: Uuid,
    meeting_type_id: Uuid,
    template_version_id: Uuid,
    mock: ZohoMock,
    _secrets_dir: tempfile::TempDir,
    provider_server: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.provider_server.abort();
    }
}

async fn login(app: &TestApp, email: &str) -> (reqwest::Client, String) {
    let client = reqwest::Client::builder()
        .cookie_store(true)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let response = client
        .post(app.url("/api/v1/auth/login"))
        .json(&json!({"email":email,"password":TEST_PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "login failed for {email}"
    );
    let csrf = TestApp::csrf_token_from(&response).expect("login did not set CSRF cookie");
    (client, csrf)
}

async fn post(
    app: &TestApp,
    client: &reqwest::Client,
    csrf: &str,
    path: &str,
    body: Value,
    idempotency_key: Option<&str>,
    if_match: Option<i64>,
) -> reqwest::Response {
    let mut request = client
        .post(app.url(path))
        .header("x-csrf-token", csrf)
        .json(&body);
    if let Some(key) = idempotency_key {
        request = request.header("idempotency-key", key);
    }
    if let Some(version) = if_match {
        request = request.header("if-match", format!("\"{version}\""));
    }
    request.send().await.unwrap()
}

async fn get(app: &TestApp, client: &reqwest::Client, path: &str) -> reqwest::Response {
    client.get(app.url(path)).send().await.unwrap()
}

async fn fixture() -> Option<Fixture> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping marketing delivery flow test");
            return None;
        }
    };
    let parsed = Url::parse(&database_url).expect("DATABASE_URL must be a postgres URL");
    assert!(
        matches!(
            parsed.path().trim_start_matches('/'),
            "outreach_zoho_delivery_flow" | "outreach_codex_native_verify"
        ),
        "this test requires a dedicated synthetic flow or workspace-verification database"
    );
    let owner = Db::connect(&database_url, None).await.unwrap();
    owner.migrate().await.unwrap();
    let db = Db::connect(&database_url, Some(ROLE_API)).await.unwrap();
    let world = seed_workspace(&db).await.unwrap();

    let mock = ZohoMock::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider_base = format!("http://{}", listener.local_addr().unwrap());
    let mock_router = Router::new()
        .fallback(any(zoho_request))
        .with_state(mock.clone());
    let provider_server = tokio::spawn(async move {
        axum::serve(listener, mock_router).await.unwrap();
    });

    let secrets_dir = tempfile::tempdir().unwrap();
    let mut config = Config::test();
    config.zoho.fake_for_tests = false;
    config.zoho.campaigns = CampaignsOAuthConfig {
        client_id: Some("1000.loopback-test".into()),
        client_secret: Some(SecretValue::new("loopback-client-secret")),
        redirect_uri: Some("http://localhost/test/callback".into()),
        allow_local_redirect: true,
    };
    config.secret_store_key = Some([0x5a; 32]);
    config.secrets_file = secrets_dir.path().join("secrets.json");
    config.objects_dir = secrets_dir.path().join("objects");
    let operations = CampaignsOperations::for_test(&provider_base).unwrap();
    let campaigns = CampaignsHttp::for_test(config.zoho.campaigns.clone(), &provider_base).unwrap();

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    sqlx::query(
        "UPDATE workspaces
            SET emergency_stop = false,
                settings = settings || jsonb_build_object('public_base_url', 'https://office.example.test')
          WHERE id = $1",
    )
    .bind(world.workspace_id)
    .execute(tx.conn())
    .await
    .unwrap();
    let connection_id: Uuid = sqlx::query_scalar(
        "INSERT INTO integration_connections
            (workspace_id, provider, region, secret_ref, scopes, status, connected_by)
         VALUES ($1, 'zoho_campaigns', 'com', $2, $3, 'active', $4)
         RETURNING id",
    )
    .bind(world.workspace_id)
    .bind("marketing-delivery-flow-secret")
    .bind(
        CAMPAIGNS_SCOPES
            .iter()
            .map(|scope| scope.to_string())
            .collect::<Vec<_>>(),
    )
    .bind(world.admin.principal_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    sqlx::query("INSERT INTO marketing_connections(id, workspace_id) VALUES ($1, $2)")
        .bind(connection_id)
        .bind(world.workspace_id)
        .execute(tx.conn())
        .await
        .unwrap();
    let sender_id: Uuid = sqlx::query_scalar(
        "INSERT INTO sender_identities
            (workspace_id, mail_account_id, address, display_name, status, verified_at)
         VALUES ($1, $2, 'news@sender.example.test', 'Office News', 'verified', now())
         RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(world.mail_account_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let channel_id: Uuid = sqlx::query_scalar(
        "INSERT INTO delivery_channels
            (workspace_id, mail_account_id, sender_identity_id, connection_id,
             provider, purpose, name, policy_status, enabled)
         VALUES ($1, $2, $3, $4, 'zoho_campaigns', 'marketing', 'Campaigns Free',
                 'allowed', true)
         RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(world.mail_account_id)
    .bind(sender_id)
    .bind(connection_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let secret_store = FileSecretStore::new(&config.secrets_file, config.secret_store_key.unwrap());
    secret_store
        .put(
            "marketing-delivery-flow-secret",
            &OAuthTokens {
                access_token: SecretValue::new("expired-access-token"),
                refresh_token: Some(SecretValue::new("loopback-refresh-token")),
                expires_at: Utc::now() - Duration::minutes(5),
            }
            .to_secret_json(),
        )
        .await
        .unwrap();
    let encrypted = std::fs::read_to_string(&config.secrets_file).unwrap();
    assert!(!encrypted.contains("expired-access-token"));
    assert!(!encrypted.contains("loopback-refresh-token"));

    let mut state = AppState::with_fake_providers(
        db.with_default_workspace(Some(world.workspace_id)),
        Arc::new(config),
        None,
    );
    state.operations = Some(Arc::new(operations));
    state.campaigns = Some(Arc::new(campaigns));
    let app = TestApp::spawn_with_state(state).await;
    let (admin, admin_csrf) = login(&app, world.admin.email.as_deref().unwrap()).await;
    let (manager, manager_csrf) = login(&app, world.manager.email.as_deref().unwrap()).await;

    let meeting_type_id = create_meeting_type(&app.state.db, &world).await;
    let template_version_id = create_office_template(&app, &admin, &admin_csrf).await;
    Some(Fixture {
        app,
        world,
        admin,
        admin_csrf,
        manager,
        manager_csrf,
        channel_id,
        meeting_type_id,
        template_version_id,
        mock,
        _secrets_dir: secrets_dir,
        provider_server,
    })
}

async fn create_meeting_type(db: &Db, world: &TestWorld) -> Uuid {
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let calendar_connection: Uuid = sqlx::query_scalar(
        "INSERT INTO integration_connections
            (workspace_id, provider, region, secret_ref, status, connected_by)
         VALUES ($1, 'zoho_calendar', 'us', 'meeting-flow-calendar', 'active', $2)
         RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(world.admin.principal_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let calendar_id: Uuid = sqlx::query_scalar(
        "INSERT INTO calendar_connections
            (workspace_id, host_principal_id, connection_id, provider_calendar_id, status)
         VALUES ($1, $2, $3, 'office-calendar', 'active') RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(world.admin.principal_id)
    .bind(calendar_connection)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let meeting_type_id: Uuid = sqlx::query_scalar(
        "INSERT INTO meeting_types
            (workspace_id, host_principal_id, calendar_connection_id, name,
             duration_minutes, buffer_before_minutes, buffer_after_minutes,
             minimum_notice_minutes, booking_horizon_days)
         VALUES ($1, $2, $3, 'Campaign appointment', 30, 0, 0, 0, 30)
         RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(world.admin.principal_id)
    .bind(calendar_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    meeting_type_id
}

async fn create_office_template(app: &TestApp, client: &reqwest::Client, csrf: &str) -> Uuid {
    let template_response = post(
        app,
        client,
        csrf,
        "/api/v1/email-templates",
        json!({"name":"Office Campaigns Flow"}),
        None,
        None,
    )
    .await;
    assert_eq!(template_response.status(), StatusCode::CREATED);
    let template = template_response.json::<Value>().await.unwrap()["data"].clone();
    let template_id = template["template_id"].as_str().unwrap();
    let response = post(
        app,
        client,
        csrf,
        &format!("/api/v1/email-templates/{template_id}/versions"),
        json!({
            "subject":"Your Office appointment",
            "body_text":"Choose a time: {{meeting.booking_url}}",
            "body_html":"<p><a href=\"{{meeting.booking_url}}\">Choose a time</a></p>",
            "variable_schema":{
                "type":"object",
                "properties":{
                    "meeting.booking_url":{"type":"string","format":"uri"}
                },
                "required":["meeting.booking_url"],
                "additionalProperties":false
            }
        }),
        None,
        None,
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::CREATED,
        "template version create failed"
    );
    response.json::<Value>().await.unwrap()["data"]["template_version_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

async fn create_recipient(db: &Db, world: &TestWorld, local_part: &str) -> Uuid {
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let address: Uuid = sqlx::query_scalar(
        "INSERT INTO email_addresses
            (workspace_id, address_original, normalized_key, kind, verification_status, verified_at)
         VALUES ($1, $2, $2, 'person', 'unknown', NULL) RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(format!("{local_part}@recipient.example.test"))
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let contact_id: Uuid =
        sqlx::query_scalar("INSERT INTO contacts(workspace_id, name) VALUES($1, $2) RETURNING id")
            .bind(world.workspace_id)
            .bind(format!("Contact {local_part}"))
            .fetch_one(tx.conn())
            .await
            .unwrap();
    sqlx::query(
        "INSERT INTO contact_emails(workspace_id, contact_id, email_address_id, is_primary)
         VALUES($1, $2, $3, true)",
    )
    .bind(world.workspace_id)
    .bind(contact_id)
    .bind(address)
    .execute(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO communication_bases
            (workspace_id, email_address_id, purpose, jurisdiction, basis_type,
             status, evidence_ref, collected_at, reviewed_by, reviewed_at)
         VALUES($1, $2, 'marketing', 'KR', 'explicit_consent', 'valid',
                'delivery-flow-consent', now(), $3, now())",
    )
    .bind(world.workspace_id)
    .bind(address)
    .bind(world.admin.principal_id)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    address
}

#[derive(Debug, Clone)]
struct LaunchSnapshot {
    id: Uuid,
    manifest_hash: String,
    approval_request_id: Uuid,
}

async fn create_frozen_launch(
    fixture: &Fixture,
    name: &str,
    recipients: &[Uuid],
) -> LaunchSnapshot {
    let response = post(
        &fixture.app,
        &fixture.admin,
        &fixture.admin_csrf,
        "/api/v1/campaigns",
        json!({
            "name":name,
            "objective":"appointment follow-up",
            "mail_account_id":fixture.world.mail_account_id,
            "delivery_channel_id":fixture.channel_id,
            "timezone":"UTC",
            "schedule_rules":{
                "weekdays":[1,2,3,4,5,6,7],
                "window_start":"00:00",
                "window_end":"23:59",
                "meeting_type_id":fixture.meeting_type_id
            },
            "steps":[{"step_order":1,"template_version_id":fixture.template_version_id,"delay_hours":0}]
        }),
        Some(&format!("{name}-campaign")),
        None,
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::CREATED,
        "Office campaign create failed"
    );
    let campaign = response.json::<Value>().await.unwrap()["data"].clone();
    let campaign_id: Uuid = campaign["campaign_id"].as_str().unwrap().parse().unwrap();
    let revision_id: Uuid = campaign["revision_id"].as_str().unwrap().parse().unwrap();

    let response = post(
        &fixture.app,
        &fixture.admin,
        &fixture.admin_csrf,
        &format!("/api/v1/campaigns/{campaign_id}/launches"),
        json!({"revision_id":revision_id,"email_address_ids":recipients}),
        Some(&format!("{name}-launch")),
        None,
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::ACCEPTED,
        "Office launch create failed"
    );
    let launch_response = response.json::<Value>().await.unwrap()["data"].clone();
    let launch_id: Uuid = launch_response["launch_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let job_id: Uuid = launch_response["job_id"].as_str().unwrap().parse().unwrap();
    freeze_launch(&fixture.app.state.db, &fixture.world, launch_id, job_id).await;

    let mut tx = fixture
        .app
        .state
        .db
        .begin(fixture.world.workspace_id)
        .await
        .unwrap();
    let (manifest_hash, request_id): (Vec<u8>, Uuid) = sqlx::query_as(
        "SELECT l.manifest_hash, a.id
           FROM campaign_launches l
           JOIN approval_requests a ON a.workspace_id=l.workspace_id AND a.launch_id=l.id
          WHERE l.workspace_id=$1 AND l.id=$2",
    )
    .bind(fixture.world.workspace_id)
    .bind(launch_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let manifest_hash = manifest_hash
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();

    LaunchSnapshot {
        id: launch_id,
        manifest_hash,
        approval_request_id: request_id,
    }
}

async fn approve_native_export(
    fixture: &Fixture,
    launch: &LaunchSnapshot,
    ready_export: &Value,
    name: &str,
) {
    let detail_response = get(
        &fixture.app,
        &fixture.manager,
        &format!("/api/v1/approvals/{}", launch.approval_request_id),
    )
    .await;
    assert_eq!(detail_response.status(), StatusCode::OK);
    let detail = detail_response.json::<Value>().await.unwrap()["data"].clone();
    assert_eq!(detail["native_export_required"], true);
    assert_eq!(detail["native_export"]["state"], "ready");
    assert_eq!(
        detail["native_export"]["export_hash"],
        ready_export["export_hash"]
    );
    let version = detail["version"].as_i64().unwrap();

    let response = post(
        &fixture.app,
        &fixture.manager,
        &fixture.manager_csrf,
        &format!("/api/v1/approvals/{}/decisions", launch.approval_request_id),
        json!({
            "decision":"approved",
            "expected_snapshot_hash":launch.manifest_hash,
            "expected_export_hash":ready_export["export_hash"]
        }),
        Some(&format!("{name}-manager-approval")),
        Some(version),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "manager export approval failed"
    );
    assert_eq!(
        response.json::<Value>().await.unwrap()["data"]["approval_status"],
        "approved"
    );

    let mut tx = fixture
        .app
        .state
        .db
        .begin(fixture.world.workspace_id)
        .await
        .unwrap();
    let dispatch_jobs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs
          WHERE workspace_id=$1 AND kind='sending.dispatch_outbound' AND state='queued'",
    )
    .bind(fixture.world.workspace_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        dispatch_jobs, 0,
        "Campaigns approval must not queue per-recipient Office sends"
    );
}

async fn freeze_launch(db: &Db, world: &TestWorld, launch_id: Uuid, job_id: Uuid) {
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let payload: Value =
        sqlx::query_scalar("SELECT payload FROM jobs WHERE workspace_id=$1 AND id=$2")
            .bind(world.workspace_id)
            .bind(job_id)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    let addresses: Vec<Uuid> = payload["email_address_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().parse().unwrap())
        .collect();
    let variables: BTreeMap<String, String> = serde_json::from_value(
        payload
            .get("variables")
            .cloned()
            .unwrap_or_else(|| json!({})),
    )
    .unwrap();
    application::campaigns::freeze_launch(
        &mut tx,
        world.system.principal_id,
        job_id,
        launch_id,
        &addresses,
        &variables,
        &payload["requester_context"],
    )
    .await
    .unwrap();
    sqlx::query(
        "UPDATE jobs SET state='completed', finished_at=now() WHERE workspace_id=$1 AND id=$2",
    )
    .bind(world.workspace_id)
    .bind(job_id)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

async fn prepare_export(fixture: &Fixture, launch: &LaunchSnapshot, name: &str) -> Value {
    let response = post(
        &fixture.app,
        &fixture.admin,
        &fixture.admin_csrf,
        "/api/v1/marketing/exports",
        json!({
            "launch_id":launch.id,
            "topic_id":null,
            "field_map":{
                "meeting.booking_url":{
                    "contact_field":CONTENT_FIELD,
                    "merge_tag":CONTENT_TAG
                },
                "compliance.unsubscribe_url":{
                    "contact_field":OFFICE_UNSUBSCRIBE_FIELD,
                    "merge_tag":OFFICE_UNSUBSCRIBE_TAG
                }
            }
        }),
        Some(&format!("{name}-prepare-export")),
        None,
    )
    .await;
    let status = response.status();
    let body = response.json::<Value>().await.unwrap();
    assert_eq!(
        status,
        StatusCode::CREATED,
        "Campaigns export prepare failed: {body}"
    );
    body["data"].clone()
}

async fn sync_export(fixture: &Fixture, export: &Value, name: &str) -> Value {
    let export_id = export["id"].as_str().unwrap();
    let response = post(
        &fixture.app,
        &fixture.admin,
        &fixture.admin_csrf,
        &format!("/api/v1/marketing/exports/{export_id}/sync"),
        json!({}),
        Some(&format!("{name}-sync-export")),
        None,
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "Campaigns export sync failed"
    );
    response.json::<Value>().await.unwrap()["data"].clone()
}

async fn content_url_from_mock_async(mock: &ZohoMock, campaign_key: &str) -> String {
    let requests = mock.requests.lock().await;
    requests
        .iter()
        .find(|request| request.path.ends_with("/createCampaign"))
        .and_then(|request| request.params.get("content_url"))
        .filter(|url| {
            Url::parse(url)
                .ok()
                .and_then(|url| {
                    url.query_pairs()
                        .find(|(key, _)| key == "signature")
                        .map(|_| ())
                })
                .is_some()
        })
        .cloned()
        .unwrap_or_else(|| panic!("no signed content_url captured for {campaign_key}"))
}

async fn booking_values_for_launch(fixture: &Fixture, launch_id: Uuid) -> HashMap<String, String> {
    let mut tx = fixture
        .app
        .state
        .db
        .begin(fixture.world.workspace_id)
        .await
        .unwrap();
    let rows: Vec<(String, Value)> = sqlx::query_as(
        "SELECT lower(a.address_original), v.variables
           FROM marketing_launch_values v
           JOIN email_addresses a ON a.workspace_id=v.workspace_id AND a.id=v.email_address_id
          WHERE v.workspace_id=$1 AND v.launch_id=$2",
    )
    .bind(fixture.world.workspace_id)
    .bind(launch_id)
    .fetch_all(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    rows.into_iter()
        .map(|(email, values)| {
            (
                email,
                values["meeting.booking_url"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

async fn sent_campaign_count(mock: &ZohoMock) -> usize {
    mock.send_calls.load(Ordering::SeqCst)
}

async fn request_count(mock: &ZohoMock, endpoint: &str) -> usize {
    mock.requests
        .lock()
        .await
        .iter()
        .filter(|request| request.path.ends_with(endpoint))
        .count()
}

async fn prior_list_read_count(mock: &ZohoMock, list_keys: &[String]) -> usize {
    let keys: HashSet<&str> = list_keys.iter().map(String::as_str).collect();
    mock.requests
        .lock()
        .await
        .iter()
        .filter(|request| {
            request.path.ends_with("/getlistsubscribers")
                && request
                    .params
                    .get("listkey")
                    .is_some_and(|key| keys.contains(key.as_str()))
        })
        .count()
}

/// Seed historical Office exports that all contain the target address. Their
/// provider lists have fresh checkpoints before the test's final-send phase;
/// each carries the same durable recipient/outbound binding the production
/// query uses to find prior native opt-outs.
async fn seed_prior_provider_lists(
    fixture: &Fixture,
    email_address_id: Uuid,
    count: usize,
) -> Vec<String> {
    let mut tx = fixture
        .app
        .state
        .db
        .begin(fixture.world.workspace_id)
        .await
        .unwrap();
    let connection_id: Uuid =
        sqlx::query_scalar("SELECT id FROM marketing_connections WHERE workspace_id=$1")
            .bind(fixture.world.workspace_id)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    let (address, normalized_address, basis_id): (String, String, Uuid) = sqlx::query_as(
        "SELECT a.address_original, a.normalized_key,
                (SELECT b.id FROM communication_bases b
                  WHERE b.workspace_id=a.workspace_id AND b.email_address_id=a.id
                    AND b.purpose='marketing' AND b.status='valid'
                  ORDER BY b.created_at DESC LIMIT 1)
           FROM email_addresses a
          WHERE a.workspace_id=$1 AND a.id=$2",
    )
    .bind(fixture.world.workspace_id)
    .bind(email_address_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let (sender_identity_id, sender_address, sender_name): (Uuid, String, String) = sqlx::query_as(
        "SELECT c.sender_identity_id, s.address, s.display_name
               FROM delivery_channels c
               JOIN sender_identities s
                 ON s.workspace_id=c.workspace_id AND s.id=c.sender_identity_id
              WHERE c.workspace_id=$1 AND c.id=$2",
    )
    .bind(fixture.world.workspace_id)
    .bind(fixture.channel_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();

    let mut list_keys = Vec::with_capacity(count);
    for index in 0..count {
        let campaign_id = campaigns::insert_campaign(
            &mut tx,
            fixture.world.mail_account_id,
            &format!("Prior provider fixture {index} {}", Uuid::new_v4()),
            Some("historical provider list fixture"),
            fixture.world.admin.principal_id,
        )
        .await
        .unwrap();
        let revision_id = campaigns::insert_revision(
            &mut tx,
            campaign_id,
            fixture.world.mail_account_id,
            fixture.channel_id,
            1,
            "UTC",
            &json!({}),
            false,
            false,
            fixture.world.admin.principal_id,
        )
        .await
        .unwrap();
        let step_id =
            campaigns::insert_step(&mut tx, revision_id, fixture.template_version_id, 1, 0)
                .await
                .unwrap();
        let launch_id = campaigns::insert_launch(
            &mut tx,
            revision_id,
            fixture.channel_id,
            fixture.world.admin.principal_id,
            Utc::now() + Duration::days(1),
        )
        .await
        .unwrap();
        let enrollment_id = campaigns::insert_enrollment(
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
        let outbound_id = campaigns::insert_step_outbound(
            &mut tx,
            fixture.world.mail_account_id,
            fixture.channel_id,
            sender_identity_id,
            email_address_id,
            Some(basis_id),
            launch_id,
            revision_id,
            enrollment_id,
            step_id,
            fixture.world.admin.principal_id,
            &sender_address,
            &address,
            "Historical subject",
            "Historical body",
            Some("<p>Historical body</p>"),
            &[0x51; 32],
        )
        .await
        .unwrap();
        let export = marketing_exports::insert_export(
            &mut tx,
            marketing_exports::NewMarketingExport {
                launch_id,
                revision_id,
                step_id,
                delivery_channel_id: fixture.channel_id,
                connection_id,
                source_manifest_hash: vec![0x51; 32],
                content_hash: vec![0x52; 32],
                audience_hash: vec![0x53; 32],
                export_hash: vec![0x54; 32],
                subject: "Historical subject".into(),
                body_text: "Historical body".into(),
                body_html: "<p>Historical body</p>".into(),
                sender_address: sender_address.clone(),
                sender_name: sender_name.clone(),
                field_map: json!({}),
                prepared_by: fixture.world.admin.principal_id,
                expires_at: Utc::now() + Duration::days(1),
            },
        )
        .await
        .unwrap();
        marketing_exports::replace_recipients(
            &mut tx,
            export.id,
            &[marketing_exports::NewExportRecipient {
                email_address_id,
                outbound_message_id: outbound_id,
                normalized_address: normalized_address.clone(),
                variables: json!({}),
                sync_status: "active".into(),
            }],
        )
        .await
        .unwrap();
        let list_key = format!("prior-list-{index}-{}", Uuid::new_v4());
        marketing_exports::update_execution_metadata(
            &mut tx,
            export.id,
            marketing_exports::ExecutionMetadata {
                provider_list_key: Some(list_key.clone()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        marketing::mark_unsubscribe_checked(&mut tx, export.id)
            .await
            .unwrap();
        list_keys.push(list_key);
    }
    tx.commit().await.unwrap();

    let mut lists = fixture.mock.lists.lock().await;
    for list_key in &list_keys {
        lists.insert(
            list_key.clone(),
            BTreeMap::from([(address.to_ascii_lowercase(), BTreeMap::new())]),
        );
    }
    list_keys
}

async fn mark_provider_unsubscribed(mock: &ZohoMock, list_key: &str, email: &str) {
    mock.unsubscribed
        .lock()
        .await
        .entry(list_key.to_string())
        .or_default()
        .insert(email.to_ascii_lowercase());
}

#[tokio::test]
async fn office_snapshot_syncs_two_bookings_and_uses_one_signed_bulk_campaign_send() {
    let Some(fixture) = fixture().await else {
        return;
    };
    let addresses = vec![
        create_recipient(&fixture.app.state.db, &fixture.world, "happy-one").await,
        create_recipient(&fixture.app.state.db, &fixture.world, "happy-two").await,
    ];
    let launch = create_frozen_launch(&fixture, "happy-path", &addresses).await;
    assert_eq!(
        launch.manifest_hash.len(),
        64,
        "approval must pin the actual Office frozen launch manifest"
    );
    let frozen_bookings = booking_values_for_launch(&fixture, launch.id).await;
    assert_eq!(frozen_bookings.len(), 2);
    assert_ne!(
        frozen_bookings.values().next().unwrap(),
        frozen_bookings.values().nth(1).unwrap(),
        "Office must mint a distinct booking capability for each recipient"
    );

    let missing_mapping = post(
        &fixture.app,
        &fixture.admin,
        &fixture.admin_csrf,
        "/api/v1/marketing/exports",
        json!({"launch_id":launch.id,"topic_id":null,"field_map":{}}),
        Some("happy-path-missing-office-unsubscribe"),
        None,
    )
    .await;
    assert_eq!(missing_mapping.status(), StatusCode::BAD_REQUEST);
    assert!(fixture.mock.lists.lock().await.is_empty());
    let prepared = prepare_export(&fixture, &launch, "happy-path").await;
    let export_id = prepared["id"].as_str().unwrap().to_string();
    let content = TestApp::data(
        get(
            &fixture.app,
            &fixture.admin,
            &format!("/api/v1/marketing/exports/{export_id}/content"),
        )
        .await,
    )
    .await;
    assert!(content["html"].as_str().unwrap().contains(CONTENT_TAG));
    assert!(
        content["html"]
            .as_str()
            .unwrap()
            .contains(OFFICE_UNSUBSCRIBE_TAG)
    );
    assert_eq!(
        content["html"]
            .as_str()
            .unwrap()
            .matches("$[LI:ORG_OPTOUT]$")
            .count(),
        1
    );

    let synced = sync_export(&fixture, &prepared, "happy-path").await;
    assert_eq!(synced["state"], "ready");
    assert_eq!(synced["recipient_count"], 2);
    assert_eq!(
        synced["active_count"], 2,
        "Zoho list readback confirms active contacts"
    );
    let mut tx = fixture
        .app
        .state
        .db
        .begin(fixture.world.workspace_id)
        .await
        .unwrap();
    let ready_poll_jobs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs
          WHERE workspace_id=$1 AND kind='marketing.poll_export'
            AND payload->>'export_id'=$2 AND state='queued'",
    )
    .bind(fixture.world.workspace_id)
    .bind(synced["id"].as_str().unwrap())
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let frozen_unsubscribes: Vec<(String, String)> = sqlx::query_as(
        "SELECT a.normalized_key, v.variables->>'compliance.unsubscribe_url'
           FROM marketing_launch_values v JOIN email_addresses a
             ON a.workspace_id=v.workspace_id AND a.id=v.email_address_id
          WHERE v.workspace_id=$1 AND v.launch_id=$2",
    )
    .bind(fixture.world.workspace_id)
    .bind(launch.id)
    .fetch_all(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        ready_poll_jobs, 1,
        "READY must atomically enqueue one first report poll"
    );
    approve_native_export(&fixture, &launch, &synced, "happy-path").await;
    let list_key = synced["provider_list_key"].as_str().unwrap();
    let provider_list = fixture
        .mock
        .lists
        .lock()
        .await
        .get(list_key)
        .cloned()
        .unwrap();
    assert_eq!(provider_list.len(), 2);
    for (email, expected_url) in &frozen_bookings {
        assert_eq!(
            provider_list[email][CONTENT_FIELD], *expected_url,
            "contact field value must stay paired with its frozen recipient"
        );
    }
    assert_eq!(frozen_unsubscribes.len(), 2);
    assert_ne!(frozen_unsubscribes[0].1, frozen_unsubscribes[1].1);
    for (email, expected_url) in &frozen_unsubscribes {
        assert_eq!(
            &provider_list[email][OFFICE_UNSUBSCRIBE_FIELD],
            expected_url
        );
        assert!(!content["html"].as_str().unwrap().contains(expected_url));
        let token = expected_url.rsplit('/').next().unwrap();
        let confirmation =
            application::compliance::public_unsubscribe::confirmation(&fixture.app.state.db, token)
                .await
                .unwrap();
        assert_eq!(confirmation.status, "pending_confirmation");
    }

    let campaign_key = synced["provider_campaign_key"]
        .as_str()
        .unwrap()
        .to_string();
    let signed_url = content_url_from_mock_async(&fixture.mock, &campaign_key).await;
    let signed_url = Url::parse(&signed_url).unwrap();
    let public_path = format!("{}?{}", signed_url.path(), signed_url.query().unwrap());
    let public_response = get(&fixture.app, &fixture.admin, &public_path).await;
    assert_eq!(
        public_response.status(),
        StatusCode::OK,
        "signed immutable Office content must resolve"
    );
    let html = public_response.text().await.unwrap();
    assert!(html.contains(CONTENT_TAG));
    assert!(html.contains("$[LI:ORG_OPTOUT]$"));
    assert!(
        !html.contains("/public/unsubscribe/"),
        "Office capability URL must not be exported"
    );

    let mut tampered = signed_url.clone();
    let query = signed_url
        .query_pairs()
        .map(|(key, value)| {
            let value = if key == "signature" {
                "tampered-signature".to_string()
            } else {
                value.into_owned()
            };
            (key.into_owned(), value)
        })
        .collect::<Vec<_>>();
    tampered.set_query(None);
    tampered.query_pairs_mut().extend_pairs(query);
    let bad_path = format!("{}?{}", tampered.path(), tampered.query().unwrap());
    assert_eq!(
        get(&fixture.app, &fixture.admin, &bad_path).await.status(),
        StatusCode::NOT_FOUND,
        "tampered signed content capability must be indistinguishable from a miss"
    );

    let send = post(
        &fixture.app,
        &fixture.admin,
        &fixture.admin_csrf,
        &format!("/api/v1/marketing/exports/{export_id}/send"),
        json!({"expected_export_hash":synced["export_hash"],"confirmation":"SEND"}),
        Some("happy-path-send"),
        None,
    )
    .await;
    assert_eq!(send.status(), StatusCode::OK, "explicit bulk SEND failed");
    let sent = send.json::<Value>().await.unwrap()["data"].clone();
    assert_eq!(sent["state"], "in_progress");
    assert_eq!(sent_campaign_count(&fixture.mock).await, 1);

    let refreshed_response = post(
        &fixture.app,
        &fixture.admin,
        &fixture.admin_csrf,
        &format!("/api/v1/marketing/exports/{export_id}/refresh"),
        json!({}),
        Some("happy-path-refresh"),
        None,
    )
    .await;
    assert_eq!(refreshed_response.status(), StatusCode::OK);
    let refreshed = refreshed_response.json::<Value>().await.unwrap()["data"].clone();
    assert_eq!(refreshed["state"], "in_progress");
    assert_eq!(refreshed["report"]["coverage_status"], "partial");
    assert_eq!(
        sent_campaign_count(&fixture.mock).await,
        1,
        "refresh is read-only"
    );

    let requests = fixture.mock.requests.lock().await;
    assert_eq!(
        requests
            .iter()
            .filter(|request| {
                request.method == Method::POST && request.path.ends_with("/createCampaign")
            })
            .count(),
        1,
        "sync should create one Campaigns draft for the shared campaign content"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| {
                request.method == Method::POST && request.path.ends_with("/addlistandcontacts")
            })
            .count(),
        1,
        "sync should create one exact provider mailing list"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| {
                request.method == Method::POST && request.path.ends_with("/json/listsubscribe")
            })
            .count(),
        2,
        "sync should apply each frozen recipient's mapped contact fields"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| {
                request.method == Method::POST && request.path.ends_with("/sendcampaign")
            })
            .count(),
        1,
        "Office must submit one bulk Campaigns send, not one send per recipient"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.path == "/oauth/v2/token")
            .count(),
        1,
        "the expired stored OAuth token must refresh through the loopback endpoint"
    );
    assert!(
        requests
            .iter()
            .filter(|request| request.path != "/oauth/v2/token")
            .all(|request| {
                request.authorization.as_deref() == Some("Zoho-oauthtoken loopback-access-token")
            })
    );
    drop(requests);
    let mut tx = fixture
        .app
        .state
        .db
        .begin(fixture.world.workspace_id)
        .await
        .unwrap();
    let dispatch_jobs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs
          WHERE workspace_id=$1 AND kind='sending.dispatch_outbound' AND state='queued'",
    )
    .bind(fixture.world.workspace_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        dispatch_jobs, 0,
        "bulk Campaigns delivery must not enqueue per-recipient sends"
    );
}

#[tokio::test]
async fn marketing_suppression_created_before_send_blocks_the_bulk_request() {
    let Some(fixture) = fixture().await else {
        return;
    };
    let address = create_recipient(&fixture.app.state.db, &fixture.world, "suppressed").await;
    let launch = create_frozen_launch(&fixture, "suppression-path", &[address]).await;
    let prepared = prepare_export(&fixture, &launch, "suppression-path").await;
    let synced = sync_export(&fixture, &prepared, "suppression-path").await;
    assert_eq!(synced["state"], "ready");
    approve_native_export(&fixture, &launch, &synced, "suppression-path").await;
    let before = sent_campaign_count(&fixture.mock).await;

    let suppression = post(
        &fixture.app,
        &fixture.admin,
        &fixture.admin_csrf,
        "/api/v1/suppressions",
        json!({
            "email_address_id":address,
            "purpose_scope":"marketing",
            "reason":"manual"
        }),
        None,
        None,
    )
    .await;
    assert_eq!(suppression.status(), StatusCode::CREATED);

    let export_id = synced["id"].as_str().unwrap();
    let response = post(
        &fixture.app,
        &fixture.admin,
        &fixture.admin_csrf,
        &format!("/api/v1/marketing/exports/{export_id}/send"),
        json!({"expected_export_hash":synced["export_hash"],"confirmation":"SEND"}),
        Some("suppression-send"),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let export = response.json::<Value>().await.unwrap()["data"].clone();
    assert_eq!(export["state"], "needs_review");
    assert_eq!(export["report"]["reason"], "recipient_suppressed");
    assert_eq!(sent_campaign_count(&fixture.mock).await, before);
    assert_eq!(
        fixture
            .mock
            .requests
            .lock()
            .await
            .iter()
            .filter(|request| request.path.ends_with("/sendcampaign"))
            .count(),
        0,
        "a new Office suppression must block before Zoho's bulk send endpoint"
    );
}

#[tokio::test]
async fn ambiguous_send_after_provider_acceptance_is_not_retried_and_refresh_reconciles_it() {
    let Some(fixture) = fixture().await else {
        return;
    };
    let address = create_recipient(&fixture.app.state.db, &fixture.world, "accepted-unknown").await;
    let launch = create_frozen_launch(&fixture, "unknown-send", &[address]).await;
    let prepared = prepare_export(&fixture, &launch, "unknown-send").await;
    let synced = sync_export(&fixture, &prepared, "unknown-send").await;
    assert_eq!(synced["state"], "ready");
    approve_native_export(&fixture, &launch, &synced, "unknown-send").await;
    let export_id = synced["id"].as_str().unwrap();
    let campaign_key = synced["provider_campaign_key"]
        .as_str()
        .unwrap()
        .to_string();
    fixture
        .mock
        .ambiguous_campaigns
        .lock()
        .await
        .insert(campaign_key.clone());

    let request_body = json!({
        "expected_export_hash":synced["export_hash"],
        "confirmation":"SEND"
    });
    let first = post(
        &fixture.app,
        &fixture.admin,
        &fixture.admin_csrf,
        &format!("/api/v1/marketing/exports/{export_id}/send"),
        request_body.clone(),
        Some("unknown-send-first"),
        None,
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK);
    let unknown = first.json::<Value>().await.unwrap()["data"].clone();
    assert_eq!(unknown["state"], "unknown");
    assert_eq!(sent_campaign_count(&fixture.mock).await, 1);

    // The loopback provider changed its own campaign to in-progress before
    // returning HTTP 500, representing accepted delivery with a lost response.
    assert_eq!(
        fixture.mock.campaigns.lock().await[&campaign_key].status,
        "inprogress"
    );
    let retry = post(
        &fixture.app,
        &fixture.admin,
        &fixture.admin_csrf,
        &format!("/api/v1/marketing/exports/{export_id}/send"),
        request_body,
        Some("unknown-send-second-key"),
        None,
    )
    .await;
    assert_eq!(retry.status(), StatusCode::OK);
    assert_eq!(
        retry.json::<Value>().await.unwrap()["data"]["state"],
        "unknown"
    );
    assert_eq!(
        sent_campaign_count(&fixture.mock).await,
        1,
        "unknown accepted sends must never be sent twice"
    );

    let refresh = post(
        &fixture.app,
        &fixture.admin,
        &fixture.admin_csrf,
        &format!("/api/v1/marketing/exports/{export_id}/refresh"),
        json!({}),
        Some("unknown-send-refresh"),
        None,
    )
    .await;
    assert_eq!(refresh.status(), StatusCode::OK);
    let reconciled = refresh.json::<Value>().await.unwrap()["data"].clone();
    assert_eq!(reconciled["state"], "in_progress");
    assert_eq!(
        reconciled["report"]["reconciliation"],
        "send_accepted_by_campaigns_readback"
    );
    assert_eq!(sent_campaign_count(&fixture.mock).await, 1);
}

#[tokio::test]
async fn approval_revoke_and_launch_stop_cannot_race_an_inflight_bulk_send() {
    let Some(fixture) = fixture().await else {
        return;
    };
    let address = create_recipient(&fixture.app.state.db, &fixture.world, "send-race").await;
    let launch = create_frozen_launch(&fixture, "send-race", &[address]).await;
    let prepared = prepare_export(&fixture, &launch, "send-race").await;
    let synced = sync_export(&fixture, &prepared, "send-race").await;
    assert_eq!(synced["state"], "ready");
    approve_native_export(&fixture, &launch, &synced, "send-race").await;

    let export_id = synced["id"].as_str().unwrap().to_string();
    let export_uuid: Uuid = export_id.parse().unwrap();
    let campaign_key = synced["provider_campaign_key"]
        .as_str()
        .unwrap()
        .to_string();
    fixture
        .mock
        .blocked_campaigns
        .lock()
        .await
        .insert(campaign_key);

    let request_received = fixture.mock.send_received.notified();
    let send_url = fixture
        .app
        .url(&format!("/api/v1/marketing/exports/{export_id}/send"));
    let send_client = fixture.admin.clone();
    let send_csrf = fixture.admin_csrf.clone();
    let send_hash = synced["export_hash"].as_str().unwrap().to_string();
    let send_task = tokio::spawn(async move {
        send_client
            .post(send_url)
            .header("x-csrf-token", send_csrf)
            .header("idempotency-key", "send-race-inflight-send")
            .json(&json!({"expected_export_hash":send_hash,"confirmation":"SEND"}))
            .send()
            .await
            .unwrap()
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), request_received)
        .await
        .expect("provider never received the bulk send request");

    let mut tx = fixture
        .app
        .state
        .db
        .begin(fixture.world.workspace_id)
        .await
        .unwrap();
    let intent: (String, String) = sqlx::query_as(
        "SELECT e.state, o.status
           FROM marketing_exports e
           JOIN marketing_operations o ON o.workspace_id=e.workspace_id AND o.export_id=e.id
          WHERE e.workspace_id=$1 AND e.id=$2
            AND o.kind='send_campaign' AND o.correlation_key='send_campaign'",
    )
    .bind(fixture.world.workspace_id)
    .bind(export_uuid)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        intent.0, "submitting",
        "write-ahead export state must commit before provider I/O"
    );
    assert_eq!(
        intent.1, "started",
        "send intent must already be durable while provider is blocked"
    );

    let approval_detail = get(
        &fixture.app,
        &fixture.manager,
        &format!("/api/v1/approvals/{}", launch.approval_request_id),
    )
    .await
    .json::<Value>()
    .await
    .unwrap()["data"]
        .clone();
    let approval_version = approval_detail["version"].as_i64().unwrap();
    let launch_response = get(
        &fixture.app,
        &fixture.admin,
        &format!("/api/v1/campaign-launches/{}", launch.id),
    )
    .await;
    let launch_version = launch_response
        .headers()
        .get("etag")
        .unwrap()
        .to_str()
        .unwrap()
        .trim_matches('"')
        .parse::<i64>()
        .unwrap();
    let launch_detail = launch_response.json::<Value>().await.unwrap()["data"].clone();
    assert_eq!(launch_detail["status"], "approved");
    let revoke_path = format!("/api/v1/approvals/{}/revoke", launch.approval_request_id);
    let pause_path = format!("/api/v1/campaign-launches/{}/pause", launch.id);
    let cancel_path = format!("/api/v1/campaign-launches/{}/cancel", launch.id);

    // All three human controls race while the already-committed send intent
    // waits on the provider. None may revoke or stop this accepted operation.
    let (revoke, pause, cancel) = tokio::join!(
        tokio::time::timeout(
            std::time::Duration::from_secs(4),
            post(
                &fixture.app,
                &fixture.manager,
                &fixture.manager_csrf,
                &revoke_path,
                json!({"reason":"race with native bulk send"}),
                Some("send-race-revoke"),
                Some(approval_version),
            ),
        ),
        tokio::time::timeout(
            std::time::Duration::from_secs(4),
            post(
                &fixture.app,
                &fixture.admin,
                &fixture.admin_csrf,
                &pause_path,
                json!({"reason":"race with native bulk send"}),
                Some("send-race-pause"),
                Some(launch_version),
            ),
        ),
        tokio::time::timeout(
            std::time::Duration::from_secs(4),
            post(
                &fixture.app,
                &fixture.admin,
                &fixture.admin_csrf,
                &cancel_path,
                json!({"reason":"race with native bulk send"}),
                Some("send-race-cancel"),
                Some(launch_version),
            ),
        ),
    );

    let mut tx = fixture
        .app
        .state
        .db
        .begin(fixture.world.workspace_id)
        .await
        .unwrap();
    let states_while_blocked: (String, String) = sqlx::query_as(
        "SELECT l.status, a.status
           FROM campaign_launches l
           JOIN approval_requests a ON a.workspace_id=l.workspace_id AND a.launch_id=l.id
          WHERE l.workspace_id=$1 AND l.id=$2",
    )
    .bind(fixture.world.workspace_id)
    .bind(launch.id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    fixture.mock.release_send.notify_one();
    let accepted = tokio::time::timeout(std::time::Duration::from_secs(5), send_task)
        .await
        .expect("in-flight provider send did not resume")
        .unwrap();

    assert_eq!(accepted.status(), StatusCode::OK);
    assert_eq!(
        accepted.json::<Value>().await.unwrap()["data"]["state"],
        "in_progress"
    );
    let concurrent_control_statuses = (
        revoke.unwrap().status(),
        pause.unwrap().status(),
        cancel.unwrap().status(),
    );
    assert_eq!(sent_campaign_count(&fixture.mock).await, 1);
    assert_eq!(
        concurrent_control_statuses,
        (
            StatusCode::CONFLICT,
            StatusCode::CONFLICT,
            StatusCode::CONFLICT
        ),
        "approval revoke, pause and cancel must all fail while SEND is in flight; launch/approval state was {states_while_blocked:?}"
    );
    assert_eq!(states_while_blocked.0, "approved");
    assert_eq!(
        states_while_blocked.1, "approved",
        "the send race must not revoke the approved decision"
    );
}

#[tokio::test]
async fn stale_prior_list_native_optout_creates_office_suppression_before_new_contact_sync() {
    let Some(fixture) = fixture().await else {
        return;
    };
    let address =
        create_recipient(&fixture.app.state.db, &fixture.world, "prior-list-optout").await;
    let first_launch = create_frozen_launch(&fixture, "prior-list-first", &[address]).await;
    let first_prepared = prepare_export(&fixture, &first_launch, "prior-list-first").await;
    let first_ready = sync_export(&fixture, &first_prepared, "prior-list-first").await;
    assert_eq!(first_ready["state"], "ready");
    approve_native_export(&fixture, &first_launch, &first_ready, "prior-list-first").await;

    let first_export_id = first_ready["id"].as_str().unwrap().to_string();
    let first_export_uuid: Uuid = first_export_id.parse().unwrap();
    let first_send = post(
        &fixture.app,
        &fixture.admin,
        &fixture.admin_csrf,
        &format!("/api/v1/marketing/exports/{first_export_id}/send"),
        json!({"expected_export_hash":first_ready["export_hash"],"confirmation":"SEND"}),
        Some("prior-list-first-send"),
        None,
    )
    .await;
    assert_eq!(first_send.status(), StatusCode::OK);
    assert_eq!(
        first_send.json::<Value>().await.unwrap()["data"]["state"],
        "in_progress"
    );

    let first_campaign_key = first_ready["provider_campaign_key"]
        .as_str()
        .unwrap()
        .to_string();
    let first_list_key = first_ready["provider_list_key"]
        .as_str()
        .unwrap()
        .to_string();
    fixture
        .mock
        .campaigns
        .lock()
        .await
        .get_mut(&first_campaign_key)
        .unwrap()
        .status = "sent".into();
    let first_refresh = post(
        &fixture.app,
        &fixture.admin,
        &fixture.admin_csrf,
        &format!("/api/v1/marketing/exports/{first_export_id}/refresh"),
        json!({}),
        Some("prior-list-first-refresh"),
        None,
    )
    .await;
    assert_eq!(first_refresh.status(), StatusCode::OK);
    assert_eq!(
        first_refresh.json::<Value>().await.unwrap()["data"]["state"],
        "completed"
    );

    // Zoho records a native opt-out after Office last read this provider list.
    let email: String = {
        let mut tx = fixture
            .app
            .state
            .db
            .begin(fixture.world.workspace_id)
            .await
            .unwrap();
        let email: String = sqlx::query_scalar(
            "SELECT lower(address_original) FROM email_addresses WHERE workspace_id=$1 AND id=$2",
        )
        .bind(fixture.world.workspace_id)
        .bind(address)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        tx.commit().await.unwrap();
        email
    };
    mark_provider_unsubscribed(&fixture.mock, &first_list_key, &email).await;
    let mut tx = fixture
        .app
        .state
        .db
        .begin(fixture.world.workspace_id)
        .await
        .unwrap();
    let (last_checked_at, unsubscribe_checked_at): (
        chrono::DateTime<Utc>,
        Option<chrono::DateTime<Utc>>,
    ) = sqlx::query_as(
        "SELECT last_checked_at, unsubscribe_checked_at
           FROM marketing_exports WHERE workspace_id=$1 AND id=$2",
    )
    .bind(fixture.world.workspace_id)
    .bind(first_export_uuid)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let recent_checkpoint = Utc::now() - Duration::minutes(5);
    assert!(last_checked_at > recent_checkpoint);
    assert!(
        unsubscribe_checked_at.is_some_and(|checked_at| checked_at > recent_checkpoint),
        "the prior list's unsubscribe checkpoint must still be fresh"
    );

    let mut tx = fixture
        .app
        .state
        .db
        .begin(fixture.world.workspace_id)
        .await
        .unwrap();
    let office_state_before_resync: (String, i64) = sqlx::query_as(
        "SELECT r.sync_status,
                (SELECT count(*) FROM suppressions s
                  WHERE s.workspace_id=r.workspace_id AND s.email_address_id=r.email_address_id
                    AND s.purpose_scope='marketing' AND s.reason='unsubscribe' AND s.released_at IS NULL)
           FROM marketing_export_recipients r
          WHERE r.workspace_id=$1 AND r.export_id=$2 AND r.email_address_id=$3",
    )
    .bind(fixture.world.workspace_id)
    .bind(first_export_uuid)
    .bind(address)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(office_state_before_resync, ("active".into(), 0));

    let before_list_creates = request_count(&fixture.mock, "/addlistandcontacts").await;
    let before_subscribes = request_count(&fixture.mock, "/json/listsubscribe").await;
    let before_campaigns = request_count(&fixture.mock, "/createCampaign").await;
    let before_sends = sent_campaign_count(&fixture.mock).await;

    // A later launch targets the same address. Its sync must scan the stale
    // prior list and mirror the opt-out before it creates a new provider list,
    // updates a contact, creates a draft, or permits another bulk send.
    let second_launch = create_frozen_launch(&fixture, "prior-list-second", &[address]).await;
    let second_prepared = prepare_export(&fixture, &second_launch, "prior-list-second").await;
    let second_sync = sync_export(&fixture, &second_prepared, "prior-list-second").await;
    assert_eq!(second_sync["state"], "needs_review");
    assert_eq!(
        second_sync["report"]["reason"],
        "provider_opt_out_mirrored_to_office"
    );
    assert_eq!(
        request_count(&fixture.mock, "/addlistandcontacts").await,
        before_list_creates
    );
    assert_eq!(
        request_count(&fixture.mock, "/json/listsubscribe").await,
        before_subscribes
    );
    assert_eq!(
        request_count(&fixture.mock, "/createCampaign").await,
        before_campaigns
    );
    assert_eq!(sent_campaign_count(&fixture.mock).await, before_sends);

    let mut tx = fixture
        .app
        .state
        .db
        .begin(fixture.world.workspace_id)
        .await
        .unwrap();
    let office_suppressions: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM suppressions
          WHERE workspace_id=$1 AND email_address_id=$2
            AND purpose_scope='marketing' AND reason='unsubscribe' AND released_at IS NULL",
    )
    .bind(fixture.world.workspace_id)
    .bind(address)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let prior_status: String = sqlx::query_scalar(
        "SELECT sync_status FROM marketing_export_recipients
          WHERE workspace_id=$1 AND export_id=$2 AND email_address_id=$3",
    )
    .bind(fixture.world.workspace_id)
    .bind(first_export_uuid)
    .bind(address)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(office_suppressions, 1);
    assert_eq!(prior_status, "unsubscribed");
}

#[tokio::test]
async fn final_send_optout_scan_resumes_across_more_than_one_hundred_prior_lists() {
    let Some(fixture) = fixture().await else {
        return;
    };
    let address = create_recipient(&fixture.app.state.db, &fixture.world, "send-scan-many").await;
    let launch = create_frozen_launch(&fixture, "send-scan-many", &[address]).await;
    let prepared = prepare_export(&fixture, &launch, "send-scan-many").await;
    let ready = sync_export(&fixture, &prepared, "send-scan-many").await;
    assert_eq!(ready["state"], "ready");
    approve_native_export(&fixture, &launch, &ready, "send-scan-many").await;
    let export_id = ready["id"].as_str().unwrap().to_string();
    let prior_lists = seed_prior_provider_lists(&fixture, address, 101).await;

    let first_response = post(
        &fixture.app,
        &fixture.admin,
        &fixture.admin_csrf,
        &format!("/api/v1/marketing/exports/{export_id}/send"),
        json!({"expected_export_hash":ready["export_hash"],"confirmation":"SEND"}),
        Some("send-scan-many-first"),
        None,
    )
    .await;
    assert_eq!(first_response.status(), StatusCode::OK);
    let first = first_response.json::<Value>().await.unwrap()["data"].clone();
    assert_eq!(first["state"], "ready");
    assert_eq!(first["report"]["readiness"], "ready");
    assert_eq!(
        first["report"]["reason"],
        "prior_provider_optout_scan_incomplete"
    );
    assert_eq!(first["report"]["send_not_submitted"], true);
    assert_eq!(first["report"]["send_scan_more"], true);
    assert_eq!(first["report"]["prior_lists_checked"], 100);
    let epoch_ms = first["report"]["send_scan_epoch_ms"].as_i64().unwrap();
    assert_eq!(sent_campaign_count(&fixture.mock).await, 0);
    assert_eq!(
        prior_list_read_count(&fixture.mock, &prior_lists).await,
        100
    );

    let mut tx = fixture
        .app
        .state
        .db
        .begin(fixture.world.workspace_id)
        .await
        .unwrap();
    let checked_before_resume: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM marketing_exports
          WHERE workspace_id=$1 AND provider_list_key=ANY($2)
            AND unsubscribe_checked_at >= to_timestamp($3::double precision / 1000.0)",
    )
    .bind(fixture.world.workspace_id)
    .bind(&prior_lists)
    .bind(epoch_ms)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(checked_before_resume, 100);
    tx.commit().await.unwrap();

    // A read-only refresh may update the summary, but it must preserve an
    // incomplete send-scan checkpoint so the next explicit SEND resumes it.
    let refresh = post(
        &fixture.app,
        &fixture.admin,
        &fixture.admin_csrf,
        &format!("/api/v1/marketing/exports/{export_id}/refresh"),
        json!({}),
        Some("send-scan-many-refresh"),
        None,
    )
    .await;
    assert_eq!(refresh.status(), StatusCode::OK);
    let refreshed = refresh.json::<Value>().await.unwrap()["data"].clone();
    assert_eq!(refreshed["state"], "ready");
    assert_eq!(refreshed["report"]["send_scan_epoch_ms"], epoch_ms);
    assert_eq!(refreshed["report"]["send_scan_more"], true);

    let second_response = post(
        &fixture.app,
        &fixture.admin,
        &fixture.admin_csrf,
        &format!("/api/v1/marketing/exports/{export_id}/send"),
        json!({"expected_export_hash":ready["export_hash"],"confirmation":"SEND"}),
        Some("send-scan-many-second"),
        None,
    )
    .await;
    assert_eq!(second_response.status(), StatusCode::OK);
    let second = second_response.json::<Value>().await.unwrap()["data"].clone();
    assert_eq!(second["state"], "in_progress");
    assert_eq!(
        prior_list_read_count(&fixture.mock, &prior_lists).await,
        101
    );
    assert_eq!(sent_campaign_count(&fixture.mock).await, 1);

    let mut tx = fixture
        .app
        .state
        .db
        .begin(fixture.world.workspace_id)
        .await
        .unwrap();
    let checked_after_resume: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM marketing_exports
          WHERE workspace_id=$1 AND provider_list_key=ANY($2)
            AND unsubscribe_checked_at >= to_timestamp($3::double precision / 1000.0)",
    )
    .bind(fixture.world.workspace_id)
    .bind(&prior_lists)
    .bind(epoch_ms)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let send_operations: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM marketing_operations
          WHERE workspace_id=$1 AND export_id=$2 AND kind='send_campaign'",
    )
    .bind(fixture.world.workspace_id)
    .bind(export_id.parse::<Uuid>().unwrap())
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(checked_after_resume, 101);
    assert_eq!(send_operations, 1);
}

#[tokio::test]
async fn workspace_booking_field_lease_blocks_overlapping_export_until_prior_campaign_completes() {
    let Some(fixture) = fixture().await else {
        return;
    };
    let shared_address =
        create_recipient(&fixture.app.state.db, &fixture.world, "lease-contact").await;
    let first_other =
        create_recipient(&fixture.app.state.db, &fixture.world, "lease-first-other").await;
    let second_other =
        create_recipient(&fixture.app.state.db, &fixture.world, "lease-second-other").await;

    let first_launch =
        create_frozen_launch(&fixture, "lease-first", &[shared_address, first_other]).await;
    let first_prepared = prepare_export(&fixture, &first_launch, "lease-first").await;
    let first_ready = sync_export(&fixture, &first_prepared, "lease-first").await;
    assert_eq!(first_ready["state"], "ready");
    approve_native_export(&fixture, &first_launch, &first_ready, "lease-first").await;
    let first_booking = booking_values_for_launch(&fixture, first_launch.id).await;
    let normalized_email: String = {
        let mut tx = fixture
            .app
            .state
            .db
            .begin(fixture.world.workspace_id)
            .await
            .unwrap();
        let email: String = sqlx::query_scalar(
            "SELECT lower(address_original) FROM email_addresses WHERE workspace_id=$1 AND id=$2",
        )
        .bind(fixture.world.workspace_id)
        .bind(shared_address)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        tx.commit().await.unwrap();
        email
    };
    let first_booking_url = first_booking[&normalized_email].clone();

    let first_export_id = first_ready["id"].as_str().unwrap();
    let first_send = post(
        &fixture.app,
        &fixture.admin,
        &fixture.admin_csrf,
        &format!("/api/v1/marketing/exports/{first_export_id}/send"),
        json!({"expected_export_hash":first_ready["export_hash"],"confirmation":"SEND"}),
        Some("lease-first-send"),
        None,
    )
    .await;
    assert_eq!(first_send.status(), StatusCode::OK);
    assert_eq!(
        first_send.json::<Value>().await.unwrap()["data"]["state"],
        "in_progress"
    );

    // Prepare another immutable Office snapshot for the same address while
    // Zoho may still be resolving the first campaign's merge values.
    let second_launch =
        create_frozen_launch(&fixture, "lease-second", &[shared_address, second_other]).await;
    let second_booking = booking_values_for_launch(&fixture, second_launch.id).await;
    let second_booking_url = second_booking[&normalized_email].clone();
    assert_ne!(
        first_booking_url, second_booking_url,
        "a later launch must have a new recipient-bound booking URL"
    );
    let second_prepared = prepare_export(&fixture, &second_launch, "lease-second").await;
    let second_export_id = second_prepared["id"].as_str().unwrap().to_string();
    let second_export_uuid: Uuid = second_export_id.parse().unwrap();
    let mut tx = fixture
        .app
        .state
        .db
        .begin(fixture.world.workspace_id)
        .await
        .unwrap();
    let mapped_booking: String = sqlx::query_scalar(
        "SELECT variables->>'Booking URL' FROM marketing_export_recipients
          WHERE workspace_id=$1 AND export_id=$2 AND email_address_id=$3",
    )
    .bind(fixture.world.workspace_id)
    .bind(second_export_uuid)
    .bind(shared_address)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(mapped_booking, second_booking_url);

    let before_list_creates = request_count(&fixture.mock, "/addlistandcontacts").await;
    let before_subscribes = request_count(&fixture.mock, "/json/listsubscribe").await;
    let before_campaigns = request_count(&fixture.mock, "/createCampaign").await;
    let second_sync = post(
        &fixture.app,
        &fixture.admin,
        &fixture.admin_csrf,
        &format!("/api/v1/marketing/exports/{second_export_id}/sync"),
        json!({}),
        Some("lease-second-sync-while-first-in-progress"),
        None,
    )
    .await;
    let problem = TestApp::problem(second_sync, StatusCode::CONFLICT).await;
    assert_eq!(problem["code"], "CONFLICT");
    assert_eq!(
        request_count(&fixture.mock, "/addlistandcontacts").await,
        before_list_creates
    );
    assert_eq!(
        request_count(&fixture.mock, "/json/listsubscribe").await,
        before_subscribes
    );
    assert_eq!(
        request_count(&fixture.mock, "/createCampaign").await,
        before_campaigns
    );
    assert_eq!(sent_campaign_count(&fixture.mock).await, 1);

    let mut tx = fixture
        .app
        .state
        .db
        .begin(fixture.world.workspace_id)
        .await
        .unwrap();
    let second_state: String =
        sqlx::query_scalar("SELECT state FROM marketing_exports WHERE workspace_id=$1 AND id=$2")
            .bind(fixture.world.workspace_id)
            .bind(second_export_uuid)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        second_state, "preparing",
        "lease rejection must precede provider writes and staging"
    );

    // Provider readback resolves the first transmission. Only then may the
    // second Office booking URL replace the provider field value.
    let first_campaign_key = first_ready["provider_campaign_key"].as_str().unwrap();
    fixture
        .mock
        .campaigns
        .lock()
        .await
        .get_mut(first_campaign_key)
        .unwrap()
        .status = "sent".into();
    let first_refresh = post(
        &fixture.app,
        &fixture.admin,
        &fixture.admin_csrf,
        &format!("/api/v1/marketing/exports/{first_export_id}/refresh"),
        json!({}),
        Some("lease-first-refresh-to-sent"),
        None,
    )
    .await;
    assert_eq!(first_refresh.status(), StatusCode::OK);
    assert_eq!(
        first_refresh.json::<Value>().await.unwrap()["data"]["state"],
        "completed"
    );

    let second_ready = sync_export(&fixture, &second_prepared, "lease-second-after-complete").await;
    assert_eq!(second_ready["state"], "ready");
    assert_eq!(
        request_count(&fixture.mock, "/addlistandcontacts").await,
        before_list_creates + 1
    );
    assert_eq!(
        request_count(&fixture.mock, "/json/listsubscribe").await,
        before_subscribes + 2
    );
    assert_eq!(
        request_count(&fixture.mock, "/createCampaign").await,
        before_campaigns + 1
    );
    let second_list_key = second_ready["provider_list_key"].as_str().unwrap();
    let provider_list = fixture
        .mock
        .lists
        .lock()
        .await
        .get(second_list_key)
        .cloned()
        .unwrap();
    assert_eq!(
        provider_list[&normalized_email][CONTENT_FIELD],
        second_booking_url
    );
    assert_eq!(sent_campaign_count(&fixture.mock).await, 1);
}

#[tokio::test]
async fn omitted_provider_fields_require_exact_audited_operator_evidence() {
    let Some(fixture) = fixture().await else {
        return;
    };
    fixture
        .mock
        .omit_custom_fields
        .store(true, Ordering::SeqCst);
    let address = create_recipient(&fixture.app.state.db, &fixture.world, "field-evidence").await;
    let launch = create_frozen_launch(&fixture, "field-evidence", &[address]).await;
    let export = prepare_export(&fixture, &launch, "field-evidence").await;
    let blocked = sync_export(&fixture, &export, "field-evidence").await;
    assert_eq!(blocked["state"], "needs_review");
    let list_key = blocked["provider_list_key"].as_str().unwrap();
    let rows = fixture.mock.lists.lock().await[list_key].clone();
    let observations: Vec<Value> = rows
        .into_iter()
        .map(|(email, fields)| json!({"email":email,"fields":fields}))
        .collect();
    let path = format!(
        "/api/v1/marketing/exports/{}/field-evidence",
        export["id"].as_str().unwrap()
    );
    let mut wrong = observations.clone();
    wrong[0]["fields"][OFFICE_UNSUBSCRIBE_FIELD] =
        json!("https://wrong.example/public/unsubscribe/wrong");
    let rejected = post(&fixture.app,&fixture.admin,&fixture.admin_csrf,&path,
        json!({"expected_export_hash":export["export_hash"],"evidence_ref":"synthetic-provider-ui","observations":wrong}),Some("field-evidence-wrong"),None).await;
    assert_eq!(
        rejected.status(),
        StatusCode::BAD_REQUEST,
        "wrong recipient capability must never be attested"
    );
    let evidence = post(&fixture.app,&fixture.admin,&fixture.admin_csrf,&path,
        json!({"expected_export_hash":export["export_hash"],"evidence_ref":"synthetic-provider-ui?token=private-capability-reference","observations":observations}),Some("field-evidence-exact"),None).await;
    assert_eq!(evidence.status(), StatusCode::OK);
    let replay=post(&fixture.app,&fixture.admin,&fixture.admin_csrf,&path,
        json!({"expected_export_hash":export["export_hash"],"evidence_ref":"synthetic-provider-ui?token=private-capability-reference","observations":observations}),Some("field-evidence-exact"),None).await;
    assert_eq!(replay.status(), StatusCode::OK);
    let mut proof_tx = fixture
        .app
        .state
        .db
        .begin(fixture.world.workspace_id)
        .await
        .unwrap();
    let proof_count:i64=sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE workspace_id=$1 AND action='marketing.export_fields_verified' AND target_id=$2")
        .bind(fixture.world.workspace_id).bind(Uuid::parse_str(export["id"].as_str().unwrap()).unwrap()).fetch_one(proof_tx.conn()).await.unwrap();
    assert_eq!(
        proof_count, 1,
        "replaying an observation must not renew its ten-minute validity"
    );
    proof_tx.rollback().await.unwrap();
    let denied=post(&fixture.app,&fixture.manager,&fixture.manager_csrf,&path,
        json!({"expected_export_hash":export["export_hash"],"evidence_ref":"not-admin","observations":observations}),Some("field-evidence-manager"),None).await;
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    fixture
        .mock
        .omit_custom_fields
        .store(false, Ordering::SeqCst);
    let email = observations[0]["email"].as_str().unwrap();
    let original = observations[0]["fields"][OFFICE_UNSUBSCRIBE_FIELD]
        .as_str()
        .unwrap()
        .to_string();
    fixture
        .mock
        .lists
        .lock()
        .await
        .get_mut(list_key)
        .unwrap()
        .get_mut(email)
        .unwrap()
        .insert(
            OFFICE_UNSUBSCRIBE_FIELD.into(),
            "https://wrong.example/unsubscribe".into(),
        );
    let contradicted = sync_export(&fixture, &export, "field-evidence-conflict").await;
    assert_eq!(
        contradicted["state"], "needs_review",
        "operator evidence must never override contradictory provider values"
    );
    fixture
        .mock
        .lists
        .lock()
        .await
        .get_mut(list_key)
        .unwrap()
        .get_mut(email)
        .unwrap()
        .insert(OFFICE_UNSUBSCRIBE_FIELD.into(), original);
    fixture
        .mock
        .omit_custom_fields
        .store(true, Ordering::SeqCst);
    let ready = sync_export(&fixture, &export, "field-evidence-after-proof").await;
    assert_eq!(ready["state"], "ready");
    approve_native_export(&fixture, &launch, &ready, "field-evidence").await;
    let sent = post(
        &fixture.app,
        &fixture.admin,
        &fixture.admin_csrf,
        &format!(
            "/api/v1/marketing/exports/{}/send",
            export["id"].as_str().unwrap()
        ),
        json!({"expected_export_hash":export["export_hash"],"confirmation":"SEND"}),
        Some("field-evidence-send"),
        None,
    )
    .await;
    assert_eq!(sent.status(), StatusCode::OK);
    let mut tx = fixture
        .app
        .state
        .db
        .begin(fixture.world.workspace_id)
        .await
        .unwrap();
    let details: Value=sqlx::query_scalar("SELECT details FROM audit_logs WHERE workspace_id=$1 AND action='marketing.export_fields_verified' AND target_id=$2")
        .bind(fixture.world.workspace_id).bind(Uuid::parse_str(export["id"].as_str().unwrap()).unwrap()).fetch_one(tx.conn()).await.unwrap();
    assert_eq!(details["source"], "operator_observation");
    assert!(
        !details.to_string().contains("private-capability-reference"),
        "even a sensitive reference must be stored only as a hash"
    );
    assert!(
        !details.to_string().contains("/public/unsubscribe/"),
        "audit stores hashes rather than capability URLs"
    );
    let export_id = Uuid::parse_str(export["id"].as_str().unwrap()).unwrap();
    let export_hash = export["export_hash"].as_str().unwrap();
    let values_hash = details["values_hash"].as_str().unwrap();
    assert!(
        marketing_exports::has_recent_field_evidence(&mut tx, export_id, export_hash, values_hash)
            .await
            .unwrap()
    );
    let mut old_details = details.clone();
    let old_values_hash = "0".repeat(64);
    old_details["values_hash"] = json!(old_values_hash);
    for when in [
        chrono::Utc::now() - chrono::Duration::minutes(11),
        chrono::Utc::now() + chrono::Duration::minutes(1),
    ] {
        sqlx::query("INSERT INTO audit_logs(workspace_id,actor_principal_id,action,target_type,target_id,request_id,details,created_at) VALUES($1,$2,'marketing.export_fields_verified','marketing_export',$3,'field-proof-window',$4,$5)")
            .bind(fixture.world.workspace_id).bind(fixture.world.admin.principal_id).bind(export_id)
            .bind(&old_details).bind(when).execute(tx.conn()).await.unwrap();
    }
    assert!(
        !marketing_exports::has_recent_field_evidence(
            &mut tx,
            export_id,
            export_hash,
            &old_values_hash
        )
        .await
        .unwrap(),
        "expired or future-dated observations cannot authorize missing fields"
    );
    marketing_exports::claim_operation(
        &mut tx,
        export_id,
        "add_contacts",
        "after-observation",
        &[7; 32],
    )
    .await
    .unwrap();
    assert!(
        !marketing_exports::has_recent_field_evidence(&mut tx, export_id, export_hash, values_hash)
            .await
            .unwrap(),
        "an unresolved later write invalidates the earlier observation"
    );
    marketing_exports::complete_operation(
        &mut tx,
        export_id,
        "add_contacts",
        "after-observation",
        None,
    )
    .await
    .unwrap();
    assert!(
        !marketing_exports::has_recent_field_evidence(&mut tx, export_id, export_hash, values_hash)
            .await
            .unwrap(),
        "a completed later write also requires a fresh observation"
    );
    tx.rollback().await.unwrap();
}
