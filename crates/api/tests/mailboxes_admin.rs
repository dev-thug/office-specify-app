//! Task 3 mailboxes API tests — T02 mailbox ACL (grantless principals get
//! no content capability), grant CRUD + preconditions, T33 Zoho marketing
//! activation block, and the OAuth connect → callback → reconnect flow.

use api::AppState;
use api::testing::TestApp;
use infrastructure::config::Config;
use infrastructure::db::{Db, ROLE_API};
use infrastructure::repo::auth as auth_repo;
use infrastructure::testing::{TEST_PASSWORD, TestPrincipal, TestWorld, seed_workspace};
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

async fn test_app() -> Option<(TestApp, TestWorld)> {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) if !u.is_empty() => u,
        _ => {
            eprintln!("DATABASE_URL not set — skipping API test");
            return None;
        }
    };
    let owner = Db::connect(&url, None).await.unwrap();
    owner.migrate().await.unwrap();
    let db = Db::connect(&url, Some(ROLE_API)).await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let db = db.with_default_workspace(Some(world.workspace_id));
    let app = TestApp::spawn_with_state(AppState::with_fake_providers(
        db,
        Arc::new(Config::test()),
        None,
    ))
    .await;
    Some((app, world))
}

async fn login_csrf(app: &TestApp, p: &TestPrincipal) -> String {
    let res = app.login(p.email.as_deref().unwrap(), TEST_PASSWORD).await;
    assert_eq!(res.status(), StatusCode::OK, "login failed");
    TestApp::csrf_token_from(&res).expect("no csrf cookie")
}

/// A second admin WITHOUT any mailbox grant (T02 subject). The seeded
/// admin holds a full grant, so create a grantless one.
async fn grantless_admin(app: &TestApp, world: &TestWorld) -> TestPrincipal {
    let email = format!(
        "admin2-{}@test.dev",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let user = auth_repo::create_user(&app.state.db, &email, "Admin NoGrant")
        .await
        .unwrap();
    let hash = infrastructure::crypto::hash_password(TEST_PASSWORD).unwrap();
    auth_repo::set_password_credential(&app.state.db, user.id, &hash)
        .await
        .unwrap();
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let pid = auth_repo::insert_principal(
        &mut tx,
        "human",
        Some(user.id),
        "Admin NoGrant",
        "admin",
        "active",
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    TestPrincipal {
        principal_id: pid,
        user_id: Some(user.id),
        email: Some(email),
        role: contracts::Role::Admin,
        kind: contracts::PrincipalKind::Human,
    }
}

async fn patch(
    app: &TestApp,
    path: &str,
    body: Value,
    csrf: &str,
    if_match: Option<&str>,
) -> reqwest::Response {
    let mut r = app
        .client
        .patch(app.url(path))
        .header("x-csrf-token", csrf)
        .json(&body);
    if let Some(v) = if_match {
        r = r.header("if-match", v);
    }
    r.send().await.unwrap()
}

async fn put_grant(
    app: &TestApp,
    account: Uuid,
    principal: Uuid,
    csrf: &str,
    precondition: Option<(&str, &str)>,
    body: Value,
) -> reqwest::Response {
    let mut r = app
        .client
        .put(app.url(&format!(
            "/api/v1/mail-accounts/{account}/grants/{principal}"
        )))
        .header("x-csrf-token", csrf)
        .json(&body);
    if let Some((k, v)) = precondition {
        r = r.header(k, v);
    }
    r.send().await.unwrap()
}

/// Insert a verified sender identity on the seeded mail account (the
/// identity CRUD belongs to another module; tests seed rows directly).
async fn seed_sender_identity(app: &TestApp, world: &TestWorld) -> Uuid {
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let id = infrastructure::repo::mailboxes::insert_sender_identity(
        &mut tx,
        world.mail_account_id,
        &format!("ops-{}@test.dev", &Uuid::new_v4().simple().to_string()[..8]),
        "Ops",
        true,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    id
}

/// Unpublished outbox events of a type for an aggregate (read via the
/// dispatcher's claim scan, then rolled back — assertions only).
async fn outbox_has(db: &Db, ws: Uuid, event_type: &str, aggregate_id: Uuid) -> bool {
    let mut tx = db.begin(ws).await.unwrap();
    let rows = infrastructure::outbox::claim_unpublished(&mut tx, 100, &[])
        .await
        .unwrap();
    let _ = tx.rollback().await;
    rows.iter()
        .any(|r| r.event_type == event_type && r.aggregate_id == aggregate_id)
}

// --------------------------------------------------------------------------
// T02 — no grant → no mailbox access capability
// --------------------------------------------------------------------------

#[tokio::test]
async fn t02_grantless_principals_get_no_mailbox_access() {
    let Some((app, world)) = test_app().await else {
        return;
    };

    // Member without grant: the account is invisible (list hides it).
    let member_csrf = login_csrf(&app, &world.member_b).await;
    let _ = member_csrf; // GET is safe — no CSRF needed
    let res = app.get("/api/v1/mail-accounts").await;
    let data = TestApp::data(res).await;
    assert_eq!(
        data.as_array().unwrap().len(),
        0,
        "grantless member must not see the account: {data}"
    );
    // ...and detail is 404 (existence hidden, §5.6).
    let res = app
        .get(&format!("/api/v1/mail-accounts/{}", world.mail_account_id))
        .await;
    let p = TestApp::problem(res, StatusCode::NOT_FOUND).await;
    assert_eq!(p["code"], "NOT_FOUND");

    // Admin WITHOUT a grant (T02 subject): management metadata is visible
    // (that is not content), but `allowed_actions` — the wire-level grant
    // capability — is EMPTY. Every content-bearing path elsewhere calls
    // `mailboxes::grant_allows`, which returns false (asserted in the
    // infra-level test).
    let admin2 = grantless_admin(&app, &world).await;
    let _admin2_csrf = login_csrf(&app, &admin2).await;
    let res = app.get("/api/v1/mail-accounts").await;
    let data = TestApp::data(res).await;
    let acct = data
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == world.mail_account_id.to_string())
        .expect("admin should see account metadata");
    assert_eq!(
        acct["allowed_actions"],
        json!([]),
        "grantless admin must have no mail.* actions"
    );
    let res = app
        .get(&format!("/api/v1/mail-accounts/{}", world.mail_account_id))
        .await;
    let data = TestApp::data(res).await;
    assert_eq!(data["allowed_actions"], json!([]));

    // The seeded admin (with full grant) sees all mail.* actions.
    let _ = login_csrf(&app, &world.admin).await;
    let res = app
        .get(&format!("/api/v1/mail-accounts/{}", world.mail_account_id))
        .await;
    let data = TestApp::data(res).await;
    for action in [
        "mail.read",
        "mail.compose",
        "mail.send",
        "mail.approve",
        "mail.assign",
    ] {
        assert!(
            data["allowed_actions"]
                .as_array()
                .unwrap()
                .contains(&json!(action)),
            "missing {action}: {data}"
        );
    }
}

// --------------------------------------------------------------------------
// grants CRUD + preconditions
// --------------------------------------------------------------------------

#[tokio::test]
async fn grant_put_preconditions_and_rules() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let csrf = login_csrf(&app, &world.admin).await;
    let acct = world.mail_account_id;
    let grantee = world.member_b.principal_id;

    let flags = |r: bool, c: bool, s: bool, a: bool, g: bool| json!({"can_read": r, "can_compose": c, "can_send": s, "can_approve": a, "can_assign": g});

    // Neither If-None-Match:* nor If-Match → 428.
    let res = put_grant(
        &app,
        acct,
        grantee,
        &csrf,
        None,
        flags(true, false, false, false, false),
    )
    .await;
    let p = TestApp::problem(res, StatusCode::PRECONDITION_REQUIRED).await;
    assert_eq!(p["code"], "VERSION_PRECONDITION_REQUIRED");

    // First grant via If-None-Match: * → 200 + ETag + permission_version.
    let res = put_grant(
        &app,
        acct,
        grantee,
        &csrf,
        Some(("if-none-match", "*")),
        flags(true, true, false, false, false),
    )
    .await;
    let grant_etag = res
        .headers()
        .get("etag")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let data = TestApp::data(res).await;
    assert_eq!(data["can_read"], true);
    assert_eq!(data["can_compose"], true);
    assert!(data["permission_version"].as_i64().unwrap() > 1);
    assert_eq!(data["pending_actions_revalidation_requested"], true);

    // Re-PUT with If-None-Match:* on an existing grant → 412.
    let res = put_grant(
        &app,
        acct,
        grantee,
        &csrf,
        Some(("if-none-match", "*")),
        flags(true, false, false, false, false),
    )
    .await;
    let p = TestApp::problem(res, StatusCode::PRECONDITION_FAILED).await;
    assert_eq!(p["code"], "VERSION_MISMATCH");

    // Update with the returned grant version.
    let res = put_grant(
        &app,
        acct,
        grantee,
        &csrf,
        Some(("if-match", &grant_etag)),
        flags(true, true, true, false, true),
    )
    .await;
    let data = TestApp::data(res).await;
    assert_eq!(data["can_send"], true);
    assert_eq!(data["can_assign"], true);

    // Dependency chain violation (send without compose) → 400.
    let res = put_grant(
        &app,
        acct,
        grantee,
        &csrf,
        Some(("if-match", "\"2\"")),
        flags(true, false, true, false, false),
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // can_approve on a Member target → 400 ROLE_REQUIRED.
    let res = put_grant(
        &app,
        acct,
        grantee,
        &csrf,
        Some(("if-match", "\"2\"")),
        flags(true, true, true, true, false),
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // An Agent may hold the mailbox can_send ACL needed by the separate
    // human-approved submit path; this does not grant direct send authority.
    let res = put_grant(
        &app,
        acct,
        world.agent.principal_id,
        &csrf,
        Some(("if-none-match", "*")),
        flags(true, true, true, false, false),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let agent_grant_etag = res
        .headers()
        .get("etag")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let agent_grant = TestApp::data(res).await;
    assert_eq!(agent_grant["can_send"], true);
    assert_eq!(agent_grant["can_approve"], false);

    // The same Agent can never receive independent approval authority.
    let res = put_grant(
        &app,
        acct,
        world.agent.principal_id,
        &csrf,
        Some(("if-match", &agent_grant_etag)),
        flags(true, true, true, true, false),
    )
    .await;
    let p = TestApp::problem(res, StatusCode::FORBIDDEN).await;
    assert_eq!(p["code"], "AGENT_DIRECT_SEND_FORBIDDEN");

    // An Agent caller cannot administer grants, even with a valid service
    // credential and a mail.read scope. Authorization is checked on the
    // authenticated actor as well as on the requested grant flags.
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let (_agent_credential_id, agent_secret) = auth_repo::create_service_credential(
        &mut tx,
        world.agent.principal_id,
        "grant-attempt",
        &["mail.read".to_string()],
        chrono::Duration::days(1),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let res = app
        .client
        .put(app.url(&format!("/api/v1/mail-accounts/{acct}/grants/{grantee}")))
        .header("authorization", format!("Bearer {agent_secret}"))
        .header("if-match", "\"2\"")
        .json(&flags(true, false, false, false, false))
        .send()
        .await
        .unwrap();
    let p = TestApp::problem(res, StatusCode::FORBIDDEN).await;
    assert_eq!(p["code"], "AGENT_DIRECT_SEND_FORBIDDEN");

    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let denied = infrastructure::repo::org::denied_audit_count(
        &mut tx,
        world.agent.principal_id,
        "mailbox.grant",
    )
    .await
    .unwrap();
    let _ = tx.rollback().await;
    assert_eq!(denied, 1, "agent grant denial must be audited");

    // Non-admin principal cannot administer grants at all.
    let member_csrf = login_csrf(&app, &world.member_a).await;
    let res = put_grant(
        &app,
        acct,
        grantee,
        &member_csrf,
        Some(("if-match", "\"2\"")),
        flags(true, false, false, false, false),
    )
    .await;
    let p = TestApp::problem(res, StatusCode::FORBIDDEN).await;
    assert_eq!(p["code"], "FORBIDDEN");

    // Grants list is admin-only and now includes member_b.
    let res = app
        .get(&format!("/api/v1/mail-accounts/{acct}/grants"))
        .await;
    let p = TestApp::problem(res, StatusCode::FORBIDDEN).await;
    assert_eq!(p["code"], "FORBIDDEN");
    let _ = login_csrf(&app, &world.admin).await;
    let res = app
        .get(&format!("/api/v1/mail-accounts/{acct}/grants"))
        .await;
    let data = TestApp::data(res).await;
    assert!(
        data.as_array()
            .unwrap()
            .iter()
            .any(|g| g["principal_id"] == grantee.to_string())
    );
}

// --------------------------------------------------------------------------
// T33 — zoho_mail marketing/transactional can never activate
// --------------------------------------------------------------------------

#[tokio::test]
async fn t33_zoho_marketing_activation_blocked() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let csrf = login_csrf(&app, &world.admin).await;
    let sender_id = seed_sender_identity(&app, &world).await;

    // Channel on the seeded zoho_mail connection, marketing purpose.
    let res = app
        .post_with_csrf(
            "/api/v1/delivery-channels",
            json!({
                "connection_id": world.connection_id,
                "mail_account_id": world.mail_account_id,
                "sender_identity_id": sender_id,
                "purpose": "marketing",
                "name": "Zoho Marketing"
            }),
            &csrf,
        )
        .await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let data = res.json::<Value>().await.unwrap()["data"].take();
    let ch_id = data["id"].as_str().unwrap().to_string();
    assert_eq!(data["policy_status"], "unverified");
    assert_eq!(data["enabled"], false);

    // Even with a positive policy review, provider/purpose policy wins.
    let res = app
        .post_with_csrf(
            &format!("/api/v1/delivery-channels/{ch_id}/reviews"),
            json!({"result": "allowed", "notes": "contract reviewed"}),
            &csrf,
        )
        .await;
    assert_eq!(res.status(), StatusCode::CREATED);

    // The review bumped the channel version (policy_status applied) —
    // refetch it, then T33: activation attempt → 422
    // PROVIDER_POLICY_BLOCKED.
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let v = infrastructure::repo::mailboxes::get_delivery_channel(
        &mut tx,
        Uuid::parse_str(&ch_id).unwrap(),
    )
    .await
    .unwrap()
    .unwrap()
    .version;
    let _ = tx.rollback().await;
    let res = patch(
        &app,
        &format!("/api/v1/delivery-channels/{ch_id}"),
        json!({"enabled": true}),
        &csrf,
        Some(&format!("\"{v}\"")),
    )
    .await;
    let p = TestApp::problem(res, StatusCode::UNPROCESSABLE_ENTITY).await;
    assert_eq!(p["code"], "PROVIDER_POLICY_BLOCKED");

    // Correspondence channel on the same Zoho connection: activation with
    // no gate evidence → 422 PROVIDER_POLICY_UNVERIFIED; after an
    // `allowed` review → 200.
    let res = app
        .post_with_csrf(
            "/api/v1/delivery-channels",
            json!({
                "connection_id": world.connection_id,
                "mail_account_id": world.mail_account_id,
                "sender_identity_id": sender_id,
                "purpose": "correspondence",
                "name": "Zoho Correspondence"
            }),
            &csrf,
        )
        .await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let data = res.json::<Value>().await.unwrap()["data"].take();
    let corr_id = data["id"].as_str().unwrap().to_string();

    let res = patch(
        &app,
        &format!("/api/v1/delivery-channels/{corr_id}"),
        json!({"enabled": true}),
        &csrf,
        Some("\"1\""),
    )
    .await;
    let p = TestApp::problem(res, StatusCode::UNPROCESSABLE_ENTITY).await;
    assert_eq!(p["code"], "PROVIDER_POLICY_UNVERIFIED");

    let res = app
        .post_with_csrf(
            &format!("/api/v1/delivery-channels/{corr_id}/reviews"),
            json!({"result": "allowed"}),
            &csrf,
        )
        .await;
    assert_eq!(res.status(), StatusCode::CREATED);

    // Post-review version (the review bumped it).
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let v = infrastructure::repo::mailboxes::get_delivery_channel(
        &mut tx,
        Uuid::parse_str(&corr_id).unwrap(),
    )
    .await
    .unwrap()
    .unwrap()
    .version;
    let _ = tx.rollback().await;
    let res = patch(
        &app,
        &format!("/api/v1/delivery-channels/{corr_id}"),
        json!({"enabled": true}),
        &csrf,
        Some(&format!("\"{v}\"")),
    )
    .await;
    let data = TestApp::data(res).await;
    assert_eq!(data["enabled"], true);

    // Non-admin cannot create channels.
    let member_csrf = login_csrf(&app, &world.member_a).await;
    let res = app
        .post_with_csrf(
            "/api/v1/delivery-channels",
            json!({
                "connection_id": world.connection_id,
                "mail_account_id": world.mail_account_id,
                "sender_identity_id": sender_id,
                "purpose": "correspondence",
                "name": "nope"
            }),
            &member_csrf,
        )
        .await;
    let p = TestApp::problem(res, StatusCode::FORBIDDEN).await;
    assert_eq!(p["code"], "FORBIDDEN");
}

// --------------------------------------------------------------------------
// OAuth connect → signed state → callback → reconnect (fake exchange)
// --------------------------------------------------------------------------

fn extract_state(url: &str) -> String {
    url.split("state=")
        .nth(1)
        .expect("authorization_url missing state param")
        .split('&')
        .next()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn oauth_connect_callback_and_reconnect() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let csrf = login_csrf(&app, &world.admin).await;

    // Connection request: authorization URL + signed state, no network.
    let res = app
        .post_with_csrf(
            "/api/v1/mail-accounts/connection-requests",
            json!({"region": "com"}),
            &csrf,
        )
        .await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let data = res.json::<Value>().await.unwrap()["data"].take();
    let url = data["authorization_url"].as_str().unwrap();
    assert!(
        url.starts_with("https://accounts.zoho.com/oauth/v2/auth"),
        "{url}"
    );
    assert!(url.contains("access_type=offline"));
    let state = extract_state(url);
    let conn_id = data["connection_id"].as_str().unwrap().to_string();

    // Tampered state → 400 before touching rows.
    let res = app
        .get(&format!(
            "/api/v1/integrations/zoho/callback?state={state}x&code=ok"
        ))
        .await;
    let p = TestApp::problem(res, StatusCode::BAD_REQUEST).await;
    assert_eq!(p["code"], "VALIDATION_FAILED");

    // Success path — the fake exchange mints tokens, the account is
    // created and the caller receives the bootstrap grant.
    let res = app
        .get(&format!(
            "/api/v1/integrations/zoho/callback?state={state}&code=ok-1"
        ))
        .await;
    let data = TestApp::data(res).await;
    assert_eq!(data["flow"], "mail");
    assert_eq!(data["connected"], true);
    let new_account = data["mail_account_id"].as_str().unwrap().to_string();

    // The connection is active; tokens live in the SecretStore only — the
    // DB row carries just `secret_ref`, never a token column.
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let conn = infrastructure::repo::mailboxes::get_connection(
        &mut tx,
        Uuid::parse_str(&conn_id).unwrap(),
    )
    .await
    .unwrap()
    .expect("connection missing");
    let acct = infrastructure::repo::mailboxes::get_mail_account(
        &mut tx,
        world.admin.principal_id,
        Uuid::parse_str(&new_account).unwrap(),
    )
    .await
    .unwrap()
    .expect("account missing");
    let _ = tx.rollback().await;
    assert_eq!(conn.status, "active");
    assert!(
        conn.provider_subject
            .unwrap_or_default()
            .starts_with("fake-subject-")
    );
    assert_eq!(acct.0.status, "active");

    // Reconnect on the seeded account → fresh signed state for the SAME
    // connection (no network).
    let res = app
        .post_with_csrf(
            &format!("/api/v1/mail-accounts/{}/reconnect", world.mail_account_id),
            json!({}),
            &csrf,
        )
        .await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let data = res.json::<Value>().await.unwrap()["data"].take();
    assert_eq!(data["connection_id"], world.connection_id.to_string());
    assert!(
        data["authorization_url"]
            .as_str()
            .unwrap()
            .contains("state=")
    );

    // Failed exchange path: `fail*` codes simulate provider denial →
    // 503 INTEGRATION_REAUTH_REQUIRED + connection reauth_required +
    // `mailbox.reauth_required` outbox event.
    let res = app
        .post_with_csrf(
            "/api/v1/mail-accounts/connection-requests",
            json!({"region": "com"}),
            &csrf,
        )
        .await;
    let data = res.json::<Value>().await.unwrap()["data"].take();
    let state = extract_state(data["authorization_url"].as_str().unwrap());
    let fail_conn = data["connection_id"].as_str().unwrap().to_string();
    let res = app
        .get(&format!(
            "/api/v1/integrations/zoho/callback?state={state}&code=fail-denied"
        ))
        .await;
    let p = TestApp::problem(res, StatusCode::SERVICE_UNAVAILABLE).await;
    assert_eq!(p["code"], "INTEGRATION_REAUTH_REQUIRED");

    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let conn = infrastructure::repo::mailboxes::get_connection(
        &mut tx,
        Uuid::parse_str(&fail_conn).unwrap(),
    )
    .await
    .unwrap()
    .expect("connection missing");
    let _ = tx.rollback().await;
    assert_eq!(conn.status, "reauth_required");
    assert!(
        outbox_has(
            &app.state.db,
            world.workspace_id,
            "mailbox.reauth_required",
            Uuid::parse_str(&fail_conn).unwrap(),
        )
        .await,
        "mailbox.reauth_required must be emitted"
    );

    // Members cannot initiate connections (admin-only).
    let member_csrf = login_csrf(&app, &world.member_a).await;
    let res = app
        .post_with_csrf(
            "/api/v1/mail-accounts/connection-requests",
            json!({"region": "com"}),
            &member_csrf,
        )
        .await;
    let p = TestApp::problem(res, StatusCode::FORBIDDEN).await;
    assert_eq!(p["code"], "FORBIDDEN");
}

// --------------------------------------------------------------------------
// calendar connections (request + list)
// --------------------------------------------------------------------------

#[tokio::test]
async fn calendar_connection_request_and_list() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let csrf = login_csrf(&app, &world.manager).await;

    // Host defaults to the caller — returns a signed OAuth start URL.
    let res = app
        .post_with_csrf(
            "/api/v1/calendar-connections/connection-requests",
            json!({}),
            &csrf,
        )
        .await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let data = res.json::<Value>().await.unwrap()["data"].take();
    assert!(
        data["authorization_url"]
            .as_str()
            .unwrap()
            .contains("ZohoCalendar")
    );

    // Naming another host requires Admin — manager is denied.
    let res = app
        .post_with_csrf(
            "/api/v1/calendar-connections/connection-requests",
            json!({"host_principal_id": world.member_b.principal_id}),
            &csrf,
        )
        .await;
    let p = TestApp::problem(res, StatusCode::FORBIDDEN).await;
    assert_eq!(p["code"], "FORBIDDEN");

    // Admin may name another host.
    let admin_csrf = login_csrf(&app, &world.admin).await;
    let res = app
        .post_with_csrf(
            "/api/v1/calendar-connections/connection-requests",
            json!({"host_principal_id": world.member_b.principal_id}),
            &admin_csrf,
        )
        .await;
    assert_eq!(res.status(), StatusCode::CREATED);

    // List: manager sees all, member_b sees only their own (none).
    let res = app.get("/api/v1/calendar-connections").await;
    let _ = TestApp::data(res).await; // manager session replaced by admin — Admin sees all
    let member_csrf = login_csrf(&app, &world.member_b).await;
    let _ = member_csrf;
    let res = app.get("/api/v1/calendar-connections").await;
    let data = TestApp::data(res).await;
    assert_eq!(data.as_array().unwrap().len(), 0);
}
