//! SMTP setup security, immutable channel binding, and RFC 8058 behavior.

use api::{AppState, testing::TestApp};
use infrastructure::{
    config::Config,
    db::{Db, ROLE_API, Tx},
    providers::{DeliveryProvider, fake::FakeDeliveryProvider},
    repo::auth as auth_repo,
    repo::compliance_core,
    testing::{TEST_PASSWORD, TestPrincipal, TestWorld, seed_workspace},
};
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};
use tempfile::TempDir;
use uuid::Uuid;

struct SmtpTestApp {
    app: TestApp,
    world: TestWorld,
    secrets: TempDir,
}

async fn setup() -> Option<SmtpTestApp> {
    let url = match std::env::var("SMTP_TEST_DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("SMTP_TEST_DATABASE_URL not set — skipping SMTP API test");
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
    assert!(
        matches!(
            database,
            "outreach_smtp_backend" | "outreach_smtp_correspondence_verify"
        ),
        "SMTP API tests must use an isolated SMTP test database"
    );
    let owner = Db::connect(&url, None).await.unwrap();
    owner.migrate().await.unwrap();
    let db = Db::connect(&url, Some(ROLE_API)).await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let secrets = tempfile::tempdir().unwrap();
    let mut config = Config::test();
    config.secrets_file = secrets.path().join("smtp-secrets.json");
    let app = TestApp::spawn_with_state(AppState::with_fake_providers(
        db.with_default_workspace(Some(world.workspace_id)),
        Arc::new(config),
        None,
    ))
    .await;
    Some(SmtpTestApp {
        app,
        world,
        secrets,
    })
}

async fn login(app: &TestApp, principal: &TestPrincipal) -> String {
    let response = app
        .login(principal.email.as_deref().unwrap(), TEST_PASSWORD)
        .await;
    assert_eq!(response.status(), StatusCode::OK, "login failed");
    TestApp::csrf_token_from(&response).expect("CSRF cookie")
}

async fn grantless_admin(app: &TestApp, world: &TestWorld) -> TestPrincipal {
    let email = format!("smtp-admin-{}@test.dev", Uuid::new_v4().simple());
    let user = auth_repo::create_user(&app.state.db, &email, "SMTP Admin NoGrant")
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
        "SMTP Admin NoGrant",
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

fn create_body(account_id: Uuid, suffix: &str) -> Value {
    json!({
        "name": "Office SMTP",
        "mail_account_id": account_id,
        "host": "smtp.example.test",
        "port": 587,
        "security": "starttls",
        "username": "smtp-user",
        "password": format!("smtp-write-only-{suffix}"),
        "from_address": format!("news-{suffix}@example.test"),
        "from_name": format!("Office News {suffix}"),
        "reply_to": null,
        "custom_headers": {"X-Importance": "high"},
        "sender_confirmed": true
    })
}

async fn post_idempotent(
    app: &TestApp,
    path: &str,
    body: &Value,
    csrf: &str,
    key: &str,
) -> reqwest::Response {
    app.client
        .post(app.url(path))
        .header("x-csrf-token", csrf)
        .header("Idempotency-Key", key)
        .json(body)
        .send()
        .await
        .unwrap()
}

async fn create_profile(app: &TestApp, world: &TestWorld, csrf: &str) -> (Value, Value) {
    let body = create_body(world.mail_account_id, &Uuid::new_v4().simple().to_string());
    let response = post_idempotent(
        app,
        "/api/v1/smtp-connections",
        &body,
        csrf,
        &format!("create-{}", Uuid::new_v4()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let response = response.json::<Value>().await.unwrap();
    (body, response["data"].clone())
}

async fn count_smtp_profiles(db: &Db, workspace_id: Uuid) -> i64 {
    let mut tx = db.begin(workspace_id).await.unwrap();
    let count = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM smtp_connections WHERE workspace_id = $1",
    )
    .bind(workspace_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    count
}

async fn suppression_count(db: &Db, workspace_id: Uuid) -> i64 {
    let mut tx = db.begin(workspace_id).await.unwrap();
    let count = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM suppressions")
        .fetch_one(tx.conn())
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    count
}

async fn insert_smtp_token(tx: &mut Tx<'_>, world: &TestWorld, profile: &Value) -> (String, Uuid) {
    let suffix = Uuid::new_v4().simple().to_string();
    let address = format!("recipient-{suffix}@example.test");
    let normalized = address.to_ascii_lowercase();
    let email_address_id: Uuid = sqlx::query_scalar(
        "INSERT INTO email_addresses (workspace_id, address_original, normalized_key)
         VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(&address)
    .bind(&normalized)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let outbound_id: Uuid = sqlx::query_scalar(
        "INSERT INTO outbound_messages
            (workspace_id, mail_account_id, delivery_channel_id, sender_identity_id,
             to_email_address_id, requested_by, purpose, from_address_snapshot,
             to_address_snapshot, subject, body_text, payload_hash)
         VALUES ($1,$2,$3,$4,$5,$6,'marketing',$7,$8,'subject','body',
                 decode(md5(random()::text),'hex'))
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(world.mail_account_id)
    .bind(Uuid::parse_str(profile["channel_id"].as_str().unwrap()).unwrap())
    .bind(Uuid::parse_str(profile["sender_identity_id"].as_str().unwrap()).unwrap())
    .bind(email_address_id)
    .bind(world.admin.principal_id)
    .bind(profile["from_address"].as_str().unwrap())
    .bind(&address)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let token = compliance_core::issue_unsubscribe_token(tx, email_address_id, outbound_id)
        .await
        .unwrap()
        .token;
    (token, outbound_id)
}

#[tokio::test]
async fn smtp_setup_is_admin_session_gated_idempotent_and_write_only() {
    let Some(test) = setup().await else { return };
    let manager_csrf = login(&test.app, &test.world.manager).await;
    let body = create_body(
        test.world.mail_account_id,
        &Uuid::new_v4().simple().to_string(),
    );
    let denied = post_idempotent(
        &test.app,
        "/api/v1/smtp-connections",
        &body,
        &manager_csrf,
        "smtp-manager-denied",
    )
    .await;
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);

    let grantless = grantless_admin(&test.app, &test.world).await;
    let grantless_csrf = login(&test.app, &grantless).await;
    let denied = post_idempotent(
        &test.app,
        "/api/v1/smtp-connections",
        &body,
        &grantless_csrf,
        "smtp-no-mailbox-grant",
    )
    .await;
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);

    let csrf = login(&test.app, &test.world.admin).await;
    let missing_key = test
        .app
        .post("/api/v1/smtp-connections", body.clone(), Some(&csrf))
        .await;
    assert_eq!(missing_key.status(), StatusCode::BAD_REQUEST);

    let no_csrf = test
        .app
        .client
        .post(test.app.url("/api/v1/smtp-connections"))
        .header("Idempotency-Key", "smtp-missing-csrf")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(no_csrf.status(), StatusCode::FORBIDDEN);

    let password = body["password"].as_str().unwrap().to_owned();
    let key = "smtp-create-idempotent";
    let created = post_idempotent(&test.app, "/api/v1/smtp-connections", &body, &csrf, key).await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let created_json = created.json::<Value>().await.unwrap();
    let profile = created_json["data"].clone();
    let profile_id = profile["id"].as_str().unwrap();
    assert_eq!(profile["status"], "pending");
    assert_eq!(profile["purpose"], "marketing");
    assert_eq!(profile["has_password"], true);
    assert!(created_json.to_string().find(&password).is_none());
    assert!(profile.get("password").is_none());
    assert!(profile.get("secret_ref").is_none());

    let listed = TestApp::data(test.app.get("/api/v1/smtp-connections").await).await;
    let listed_profile = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == profile["id"])
        .expect("created SMTP profile should be listed");
    assert_eq!(listed_profile["has_password"], true);
    assert!(listed.to_string().find(&password).is_none());
    assert!(listed.to_string().find("secret_ref").is_none());

    let replay = post_idempotent(&test.app, "/api/v1/smtp-connections", &body, &csrf, key).await;
    assert_eq!(replay.status(), StatusCode::CREATED);
    let replay_json = replay.json::<Value>().await.unwrap();
    assert_eq!(replay_json["data"]["id"], profile["id"]);
    assert_eq!(
        count_smtp_profiles(&test.app.state.db, test.world.workspace_id).await,
        1
    );

    let mut incompatible_sender = body.clone();
    incompatible_sender["name"] = json!("Second Office SMTP");
    incompatible_sender["from_name"] = json!("Different display name");
    let incompatible = post_idempotent(
        &test.app,
        "/api/v1/smtp-connections",
        &incompatible_sender,
        &csrf,
        "smtp-incompatible-sender",
    )
    .await;
    assert_eq!(incompatible.status(), StatusCode::CONFLICT);
    assert_eq!(
        count_smtp_profiles(&test.app.state.db, test.world.workspace_id).await,
        1
    );

    let world2 = seed_workspace(&test.app.state.db).await.unwrap();
    let app2 = TestApp::spawn_with_state(AppState::with_fake_providers(
        test.app
            .state
            .db
            .clone()
            .with_default_workspace(Some(world2.workspace_id)),
        test.app.state.config.clone(),
        None,
    ))
    .await;
    let csrf2 = login(&app2, &world2.admin).await;
    let (_, profile2) = create_profile(&app2, &world2, &csrf2).await;
    let cross_workspace = post_idempotent(
        &test.app,
        &format!(
            "/api/v1/smtp-connections/{}/disable",
            profile2["id"].as_str().unwrap()
        ),
        &json!({}),
        &csrf,
        "smtp-cross-workspace-disable",
    )
    .await;
    assert_eq!(cross_workspace.status(), StatusCode::NOT_FOUND);

    let mut different_body = body.clone();
    different_body["password"] = json!("different-secret");
    let reused = post_idempotent(
        &test.app,
        "/api/v1/smtp-connections",
        &different_body,
        &csrf,
        key,
    )
    .await;
    assert_eq!(reused.status(), StatusCode::CONFLICT);

    let channel_id = Uuid::parse_str(profile["channel_id"].as_str().unwrap()).unwrap();
    let mut tx = test
        .app
        .state
        .db
        .begin(test.world.workspace_id)
        .await
        .unwrap();
    let (connection_status, channel_enabled, policy_status, notes, evidence_ref): (
        String,
        bool,
        String,
        Option<String>,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT sc.status, dc.enabled, dc.policy_status, cr.notes, cr.contract_evidence_ref
           FROM smtp_connections sc
           JOIN delivery_channels dc ON dc.workspace_id = sc.workspace_id AND dc.id = sc.channel_id
           JOIN channel_reviews cr ON cr.workspace_id = sc.workspace_id
                AND cr.delivery_channel_id = dc.id
          WHERE sc.workspace_id = $1 AND sc.id = $2",
    )
    .bind(test.world.workspace_id)
    .bind(Uuid::parse_str(profile_id).unwrap())
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(connection_status, "pending");
    assert!(!channel_enabled);
    assert_eq!(policy_status, "allowed");
    assert!(notes.unwrap().contains("Administrator attested"));
    assert!(
        evidence_ref
            .unwrap()
            .starts_with("smtp-sender-attestation:")
    );

    let audit_details: serde_json::Value = sqlx::query_scalar(
        "SELECT details FROM audit_logs WHERE workspace_id = $1
          AND action = 'smtp_connection.create' AND target_id = $2",
    )
    .bind(test.world.workspace_id)
    .bind(Uuid::parse_str(profile_id).unwrap())
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert!(!audit_details.to_string().contains(&password));
    tx.commit().await.unwrap();

    let secret_path: PathBuf = test.secrets.path().join("smtp-secrets.json");
    let encrypted_secrets = std::fs::read_to_string(secret_path).unwrap();
    assert!(!encrypted_secrets.contains(&password));

    let disabled = post_idempotent(
        &test.app,
        &format!("/api/v1/smtp-connections/{profile_id}/disable"),
        &json!({}),
        &csrf,
        "smtp-disable-idempotent",
    )
    .await;
    assert_eq!(disabled.status(), StatusCode::OK);
    let disabled = disabled.json::<Value>().await.unwrap()["data"].clone();
    assert_eq!(disabled["status"], "disabled");
    assert_eq!(disabled["purpose"], "marketing");
    assert_eq!(disabled["has_password"], false);
    assert!(
        !std::fs::read_to_string(test.secrets.path().join("smtp-secrets.json"))
            .unwrap()
            .contains(&password)
    );

    let mut tx = test
        .app
        .state
        .db
        .begin(test.world.workspace_id)
        .await
        .unwrap();
    let (connection_status, channel_enabled, integration_status): (String, bool, String) =
        sqlx::query_as(
            "SELECT sc.status, dc.enabled, ic.status
               FROM smtp_connections sc
               JOIN delivery_channels dc ON dc.workspace_id = sc.workspace_id AND dc.id = sc.channel_id
               JOIN integration_connections ic ON ic.workspace_id = sc.workspace_id AND ic.id = sc.id
              WHERE sc.workspace_id = $1 AND sc.id = $2",
        )
        .bind(test.world.workspace_id)
        .bind(Uuid::parse_str(profile_id).unwrap())
        .fetch_one(tx.conn())
        .await
        .unwrap();
    assert_eq!(connection_status, "disabled");
    assert!(!channel_enabled);
    assert_eq!(integration_status, "disabled");
    assert_ne!(channel_id, Uuid::nil());
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn smtp_correspondence_profiles_keep_channel_purpose_in_summary() {
    let Some(test) = setup().await else { return };
    let csrf = login(&test.app, &test.world.admin).await;
    let mut body = create_body(
        test.world.mail_account_id,
        &Uuid::new_v4().simple().to_string(),
    );
    body["purpose"] = json!("correspondence");

    let created = post_idempotent(
        &test.app,
        "/api/v1/smtp-connections",
        &body,
        &csrf,
        "smtp-create-correspondence",
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let profile = created.json::<Value>().await.unwrap()["data"].clone();
    assert_eq!(profile["purpose"], "correspondence");

    let profile_id = Uuid::parse_str(profile["id"].as_str().unwrap()).unwrap();
    let channel_id = Uuid::parse_str(profile["channel_id"].as_str().unwrap()).unwrap();
    let mut tx = test
        .app
        .state
        .db
        .begin(test.world.workspace_id)
        .await
        .unwrap();
    let bound_purpose: String = sqlx::query_scalar(
        "SELECT dc.purpose FROM smtp_connections sc
           JOIN delivery_channels dc ON dc.workspace_id = sc.workspace_id
                AND dc.id = sc.channel_id
          WHERE sc.workspace_id = $1 AND sc.id = $2 AND dc.id = $3",
    )
    .bind(test.world.workspace_id)
    .bind(profile_id)
    .bind(channel_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(bound_purpose, "correspondence");
    let repurpose_result = sqlx::query(
        "UPDATE delivery_channels SET purpose = 'marketing'
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(test.world.workspace_id)
    .bind(channel_id)
    .execute(tx.conn())
    .await;
    assert!(
        repurpose_result.is_err(),
        "an SMTP-bound channel purpose must stay immutable"
    );
    tx.rollback().await.unwrap();

    let listed = TestApp::data(test.app.get("/api/v1/smtp-connections").await).await;
    let listed_profile = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == profile["id"])
        .expect("created correspondence profile should be listed");
    assert_eq!(listed_profile["purpose"], "correspondence");

    let mut unsupported_body = create_body(
        test.world.mail_account_id,
        &Uuid::new_v4().simple().to_string(),
    );
    unsupported_body["purpose"] = json!("transactional");
    let unsupported = post_idempotent(
        &test.app,
        "/api/v1/smtp-connections",
        &unsupported_body,
        &csrf,
        "smtp-create-transactional-rejected",
    )
    .await;
    assert_eq!(unsupported.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        count_smtp_profiles(&test.app.state.db, test.world.workspace_id).await,
        1
    );
}

#[tokio::test]
async fn smtp_one_click_is_form_validated_replay_safe_and_survives_disable() {
    let Some(test) = setup().await else { return };
    let csrf = login(&test.app, &test.world.admin).await;
    let (_body, profile) = create_profile(&test.app, &test.world, &csrf).await;
    let profile_id = Uuid::parse_str(profile["id"].as_str().unwrap()).unwrap();
    let mut tx = test
        .app
        .state
        .db
        .begin(test.world.workspace_id)
        .await
        .unwrap();
    // The real verification endpoint is transport-only; mark the profile
    // active here so this test exercises an already-sent SMTP token without
    // contacting any SMTP service.
    sqlx::query(
        "UPDATE integration_connections SET status = 'active' WHERE workspace_id = $1 AND id = $2",
    )
    .bind(test.world.workspace_id)
    .bind(profile_id)
    .execute(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE smtp_connections SET status = 'active' WHERE workspace_id = $1 AND id = $2",
    )
    .bind(test.world.workspace_id)
    .bind(profile_id)
    .execute(tx.conn())
    .await
    .unwrap();
    sqlx::query("UPDATE delivery_channels SET enabled = true WHERE workspace_id = $1 AND id = $2 AND policy_status = 'allowed'")
        .bind(test.world.workspace_id)
        .bind(Uuid::parse_str(profile["channel_id"].as_str().unwrap()).unwrap())
        .execute(tx.conn())
        .await
        .unwrap();
    let channel_id = Uuid::parse_str(profile["channel_id"].as_str().unwrap()).unwrap();
    assert_eq!(
        infrastructure::repo::sending::channel_last_accepted_at(&mut tx, channel_id)
            .await
            .unwrap(),
        None,
        "an SMTP channel with no accepted messages has no last-accepted timestamp"
    );
    let (token, outbound_id) = insert_smtp_token(&mut tx, &test.world, &profile).await;
    sqlx::query(
        "UPDATE outbound_messages
            SET status = 'accepted', accepted_at = '2026-10-10T12:34:56.123456Z',
                provider_message_id = 'smtp-fixture-message'
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(test.world.workspace_id)
    .bind(outbound_id)
    .execute(tx.conn())
    .await
    .unwrap();
    assert!(
        infrastructure::repo::sending::channel_last_accepted_at(&mut tx, channel_id)
            .await
            .unwrap()
            .is_some(),
        "an accepted SMTP message should produce a last-accepted timestamp"
    );
    tx.commit().await.unwrap();

    let clone_channel = test
        .app
        .post(
            "/api/v1/delivery-channels",
            json!({
                "connection_id": profile_id,
                "mail_account_id": test.world.mail_account_id,
                "sender_identity_id": profile["sender_identity_id"],
                "purpose": "marketing",
                "name": "SMTP sibling channel"
            }),
            Some(&csrf),
        )
        .await;
    assert_eq!(clone_channel.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let fallback: Arc<dyn DeliveryProvider> = Arc::new(FakeDeliveryProvider::new());
    let delivery_router = application::smtp::DeliveryRouter::new(
        test.app.state.db.clone(),
        test.app.state.config.clone(),
        fallback,
    );
    let routed = delivery_router
        .for_channel(test.world.workspace_id, channel_id)
        .await
        .unwrap()
        .expect("active SMTP profile should bind by workspace and channel");
    assert!(routed.capabilities().supports_send);

    let orphan_connection_id = Uuid::new_v4();
    let orphan_channel_id = Uuid::new_v4();
    let mut tx = test
        .app
        .state
        .db
        .begin(test.world.workspace_id)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO integration_connections
            (id, workspace_id, provider, region, secret_ref, status, connected_by)
         VALUES ($1,$2,'delivery_provider','smtp','secret://smtp/orphan','active',$3)",
    )
    .bind(orphan_connection_id)
    .bind(test.world.workspace_id)
    .bind(test.world.admin.principal_id)
    .execute(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO delivery_channels
            (id, workspace_id, mail_account_id, sender_identity_id, connection_id,
             provider, purpose, name, policy_status, enabled)
         VALUES ($1,$2,$3,$4,$5,'delivery_provider','marketing','orphan SMTP channel','allowed',true)",
    )
    .bind(orphan_channel_id)
    .bind(test.world.workspace_id)
    .bind(test.world.mail_account_id)
    .bind(Uuid::parse_str(profile["sender_identity_id"].as_str().unwrap()).unwrap())
    .bind(orphan_connection_id)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let orphan_route = delivery_router
        .for_channel(test.world.workspace_id, orphan_channel_id)
        .await
        .unwrap()
        .expect("SMTP-backed channel without a profile must fail closed");
    assert!(!orphan_route.capabilities().supports_send);

    assert!(
        delivery_router
            .for_channel(Uuid::new_v4(), channel_id)
            .await
            .unwrap()
            .is_none()
    );

    let invalid = test
        .app
        .client
        .post(
            test.app
                .url(&format!("/public/v1/unsubscribe/{token}/one-click")),
        )
        .header("content-type", "application/x-www-form-urlencoded")
        .body("List-Unsubscribe=Wrong")
        .send()
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        suppression_count(&test.app.state.db, test.world.workspace_id).await,
        0
    );

    let disabled = post_idempotent(
        &test.app,
        &format!("/api/v1/smtp-connections/{profile_id}/disable"),
        &json!({}),
        &csrf,
        "smtp-disable-before-unsubscribe",
    )
    .await;
    assert_eq!(disabled.status(), StatusCode::OK);
    let disabled_route = delivery_router
        .for_channel(test.world.workspace_id, channel_id)
        .await
        .unwrap()
        .expect("disabled SMTP profile must fail closed");
    assert!(!disabled_route.capabilities().supports_send);

    let post_one_click = || {
        test.app
            .client
            .post(
                test.app
                    .url(&format!("/public/v1/unsubscribe/{token}/one-click")),
            )
            .header("content-type", "application/x-www-form-urlencoded")
            .body("List-Unsubscribe=One-Click")
    };
    let first = post_one_click().send().await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(
        first.json::<Value>().await.unwrap()["data"]["status"],
        "unsubscribed"
    );
    assert_eq!(
        suppression_count(&test.app.state.db, test.world.workspace_id).await,
        1
    );

    let replay = post_one_click().send().await.unwrap();
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(
        replay.json::<Value>().await.unwrap()["data"]["status"],
        "unsubscribed"
    );
    assert_eq!(
        suppression_count(&test.app.state.db, test.world.workspace_id).await,
        1
    );
}
