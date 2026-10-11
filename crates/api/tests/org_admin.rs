//! Task 3 org API tests — admin surface (principals, member-invitations,
//! agents, service credentials, disable-requests, emergency-stop) and the
//! T03 agent-restriction matrix (403 + denied audit row).

use api::AppState;
use api::testing::TestApp;
use infrastructure::config::Config;
use infrastructure::db::{Db, ROLE_API};
use infrastructure::testing::{TEST_PASSWORD, TestWorld, seed_workspace};
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

/// Login as the world admin; returns the CSRF token for session mutations.
async fn admin_csrf(app: &TestApp, world: &TestWorld) -> String {
    let res = app
        .login(world.admin.email.as_deref().unwrap(), TEST_PASSWORD)
        .await;
    assert_eq!(res.status(), StatusCode::OK, "admin login failed");
    TestApp::csrf_token_from(&res).expect("no csrf cookie")
}

/// Login as an arbitrary seeded human principal.
async fn login_csrf(app: &TestApp, p: &infrastructure::testing::TestPrincipal) -> String {
    let res = app.login(p.email.as_deref().unwrap(), TEST_PASSWORD).await;
    assert_eq!(res.status(), StatusCode::OK, "login failed");
    TestApp::csrf_token_from(&res).expect("no csrf cookie")
}

fn session_token_from(res: &reqwest::Response) -> String {
    res.headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|s| s.split(';').next()?.split_once('='))
        .find(|(key, _)| *key == "outreach_session")
        .map(|(_, value)| value.to_string())
        .expect("session cookie missing")
}

async fn create_session(db: &Db, workspace_id: Uuid, principal_id: Uuid) -> (Uuid, String) {
    let mut tx = db.begin(workspace_id).await.unwrap();
    let (id, token) = infrastructure::repo::auth::create_session(
        &mut tx,
        principal_id,
        chrono::Duration::hours(1),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    (id, token)
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

async fn put(
    app: &TestApp,
    path: &str,
    body: Value,
    csrf: &str,
    extra: &[(&str, &str)],
) -> reqwest::Response {
    let mut r = app
        .client
        .put(app.url(path))
        .header("x-csrf-token", csrf)
        .json(&body);
    for (k, v) in extra {
        r = r.header(*k, *v);
    }
    r.send().await.unwrap()
}

async fn bearer(
    app: &TestApp,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
    secret: &str,
    extra: &[(&str, &str)],
) -> reqwest::Response {
    let mut r = app
        .client
        .request(method, app.url(path))
        .header("authorization", format!("Bearer {secret}"));
    for (k, v) in extra {
        r = r.header(*k, *v);
    }
    if let Some(b) = body {
        r = r.json(&b);
    }
    r.send().await.unwrap()
}

async fn post_idem(
    app: &TestApp,
    path: &str,
    body: Value,
    csrf: &str,
    key: Option<&str>,
) -> reqwest::Response {
    let mut r = app
        .client
        .post(app.url(path))
        .header("x-csrf-token", csrf)
        .json(&body);
    if let Some(k) = key {
        r = r.header("idempotency-key", k);
    }
    r.send().await.unwrap()
}

/// Count `denied:*` audit rows for an actor/action pair (T03 proof).
async fn denied_audits(db: &Db, ws: Uuid, actor: Uuid, action: &str) -> i64 {
    let mut tx = db.begin(ws).await.unwrap();
    let n = infrastructure::repo::org::denied_audit_count(&mut tx, actor, action)
        .await
        .unwrap();
    let _ = tx.rollback().await;
    n
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
// principals list/get/patch + versioning
// --------------------------------------------------------------------------

#[tokio::test]
async fn principals_list_get_patch_versioning() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let csrf = admin_csrf(&app, &world).await;

    // List: seeded humans + agent + system.
    let res = app.get("/api/v1/principals").await;
    let data = TestApp::data(res).await;
    let items = data.as_array().unwrap();
    assert!(items.len() >= 6, "expected seeded principals: {data}");
    assert!(
        items
            .iter()
            .any(|p| p["id"] == world.admin.principal_id.to_string())
    );

    // Filter to humans.
    let res = app.get("/api/v1/principals?kind=human").await;
    let data = TestApp::data(res).await;
    assert!(
        data.as_array()
            .unwrap()
            .iter()
            .all(|p| p["kind"] == "human")
    );

    // Detail carries an ETag equal to version.
    let path = format!("/api/v1/principals/{}", world.member_b.principal_id);
    let res = app.get(&path).await;
    let etag = res
        .headers()
        .get("etag")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let data = TestApp::data(res).await;
    assert_eq!(etag, format!("\"{}\"", data["version"].as_i64().unwrap()));

    // PATCH without If-Match → 428; stale → 412; correct → 200 + new etag.
    let res = patch(&app, &path, json!({"role": "manager"}), &csrf, None).await;
    let p = TestApp::problem(res, StatusCode::PRECONDITION_REQUIRED).await;
    assert_eq!(p["code"], "VERSION_PRECONDITION_REQUIRED");

    let res = patch(
        &app,
        &path,
        json!({"role": "manager"}),
        &csrf,
        Some("\"999\""),
    )
    .await;
    let p = TestApp::problem(res, StatusCode::PRECONDITION_FAILED).await;
    assert_eq!(p["code"], "VERSION_MISMATCH");

    let res = patch(&app, &path, json!({"role": "manager"}), &csrf, Some(&etag)).await;
    let new_etag = res
        .headers()
        .get("etag")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let data = TestApp::data(res).await;
    assert_eq!(data["role"], "manager");
    assert_ne!(etag, new_etag, "version must advance");

    // Deactivation requires the asynchronous A.7 cleanup request so owned
    // work is handed off before the principal is disabled.
    let res = patch(
        &app,
        &path,
        json!({"status": "disabled"}),
        &csrf,
        Some(&new_etag),
    )
    .await;
    let p = TestApp::problem(res, StatusCode::BAD_REQUEST).await;
    assert_eq!(p["code"], "VALIDATION_FAILED");

    // Members cannot patch principals.
    let member_csrf = login_csrf(&app, &world.member_a).await;
    let res = app.get("/api/v1/principals").await;
    let picker = TestApp::data(res).await;
    let picker_rows = picker.as_array().unwrap();
    assert!(!picker_rows.is_empty());
    assert!(picker_rows.iter().all(|p| p["kind"] == "human"));
    for row in picker_rows {
        let mut keys = row
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        keys.sort_unstable();
        assert_eq!(keys, ["id", "kind", "name", "role", "status"]);
        assert_eq!(row["status"], "active");
    }

    let res = app.get(&path).await;
    let p = TestApp::problem(res, StatusCode::FORBIDDEN).await;
    assert_eq!(p["code"], "FORBIDDEN");
    assert!(
        denied_audits(
            &app.state.db,
            world.workspace_id,
            world.member_a.principal_id,
            "principal.read",
        )
        .await
            >= 1
    );

    let res = patch(
        &app,
        &path,
        json!({"role": "admin"}),
        &member_csrf,
        Some(&new_etag),
    )
    .await;
    let p = TestApp::problem(res, StatusCode::FORBIDDEN).await;
    assert_eq!(p["code"], "FORBIDDEN");

    // Privilege escalation is also blocked on the way out: members can't
    // patch at all (403 above) and an admin patching a member to `admin`
    // is allowed but audited (role change is a real action, not a denial).
    let denied = denied_audits(
        &app.state.db,
        world.workspace_id,
        world.member_a.principal_id,
        "principal.update",
    )
    .await;
    assert!(denied >= 1, "member's denied patch must be audited");
}

#[tokio::test]
async fn agent_credential_reads_principal_roster_and_detail() {
    // Agent/human parity: an agent credential carrying `org.admin` holds
    // Admin-equivalent authority and may read the roster and a detail.
    let Some((app, world)) = test_app().await else {
        return;
    };
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let (_credential_id, secret) = infrastructure::repo::auth::create_service_credential(
        &mut tx,
        world.agent.principal_id,
        "org-admin",
        &["org.admin".to_string()],
        chrono::Duration::days(1),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let res = bearer(
        &app,
        reqwest::Method::GET,
        "/api/v1/principals",
        None,
        &secret,
        &[],
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = res.json().await.unwrap();
    assert!(
        body["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == world.member_a.principal_id.to_string()),
        "roster must include member_a: {body}"
    );
    // org.admin is treated like Admin: the full roster, agent rows included.
    assert!(
        body["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == world.agent.principal_id.to_string()),
        "org.admin roster must include agent principals: {body}"
    );

    let detail_path = format!("/api/v1/principals/{}", world.member_a.principal_id);
    let res = bearer(&app, reqwest::Method::GET, &detail_path, None, &secret, &[]).await;
    let data = TestApp::data(res).await;
    assert_eq!(data["id"], world.member_a.principal_id.to_string());
}

#[tokio::test]
async fn agent_without_org_admin_gets_picker_only_and_cannot_read_principal_detail() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let (_credential_id, secret) = infrastructure::repo::auth::create_service_credential(
        &mut tx,
        world.agent.principal_id,
        "no-org-admin",
        &["mail.read".to_string()],
        chrono::Duration::days(1),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    // Like a Member, a plain agent gets only the active-human picker view
    // of the list (no agent rows)...
    let res = bearer(
        &app,
        reqwest::Method::GET,
        "/api/v1/principals",
        None,
        &secret,
        &[],
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = res.json().await.unwrap();
    assert!(
        !body["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == world.agent.principal_id.to_string()),
        "picker must not list agent principals: {body}"
    );

    // ...and the admin-only principal detail is denied and audited.
    let detail_path = format!("/api/v1/principals/{}", world.member_a.principal_id);
    let res = bearer(&app, reqwest::Method::GET, &detail_path, None, &secret, &[]).await;
    let p = TestApp::problem(res, StatusCode::FORBIDDEN).await;
    assert_eq!(p["code"], "FORBIDDEN");
    assert!(
        denied_audits(
            &app.state.db,
            world.workspace_id,
            world.agent.principal_id,
            "principal.read"
        )
        .await
            >= 1,
        "missing denied audit for principal.read"
    );
}

#[tokio::test]
async fn me_sessions_are_self_only_redacted_and_revoke_is_idempotent() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let login = app
        .login(world.admin.email.as_deref().unwrap(), TEST_PASSWORD)
        .await;
    assert_eq!(login.status(), StatusCode::OK);
    let csrf = TestApp::csrf_token_from(&login).unwrap();
    let current_token = session_token_from(&login);
    let current_session = infrastructure::repo::auth::lookup_session(&app.state.db, &current_token)
        .await
        .unwrap()
        .unwrap()
        .session
        .id;
    let (other_session, _) =
        create_session(&app.state.db, world.workspace_id, world.admin.principal_id).await;
    let (foreign_session, _) = create_session(
        &app.state.db,
        world.workspace_id,
        world.member_a.principal_id,
    )
    .await;

    let res = app.get("/api/v1/me/sessions").await;
    let data = TestApp::data(res).await;
    let sessions = data.as_array().unwrap();
    assert_eq!(sessions.len(), 2);
    assert!(
        sessions
            .iter()
            .any(|row| { row["id"] == current_session.to_string() && row["current"] == true })
    );
    assert!(
        sessions
            .iter()
            .any(|row| { row["id"] == other_session.to_string() && row["current"] == false })
    );
    for row in sessions {
        let mut keys = row
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "created_at",
                "current",
                "expires_at",
                "id",
                "last_seen_at",
                "revoked_at"
            ]
        );
        assert!(row.get("session_hash").is_none());
    }

    // The id belongs to another principal, so existence is hidden.
    let foreign_path = format!("/api/v1/me/sessions/{foreign_session}/revoke");
    let res = app
        .client
        .post(app.url(&foreign_path))
        .header("x-csrf-token", &csrf)
        .header("idempotency-key", Uuid::new_v4().to_string())
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    let p = TestApp::problem(res, StatusCode::NOT_FOUND).await;
    assert_eq!(p["code"], "NOT_FOUND");

    // An own non-current session can be revoked and replayed safely.
    let path = format!("/api/v1/me/sessions/{other_session}/revoke");
    let key = format!("session-{}", Uuid::new_v4());
    let request = || {
        app.client
            .post(app.url(&path))
            .header("x-csrf-token", &csrf)
            .header("idempotency-key", &key)
            .json(&json!({}))
    };
    let res = request().send().await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let first = res.json::<Value>().await.unwrap()["data"].take();
    assert_eq!(first["id"], other_session.to_string());
    assert!(first["revoked_at"].is_string());
    let res = request().send().await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let replay = res.json::<Value>().await.unwrap()["data"].take();
    assert_eq!(replay, first);

    // Agents and bearer callers cannot use personal-session management.
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let (_credential_id, secret) = infrastructure::repo::auth::create_service_credential(
        &mut tx,
        world.agent.principal_id,
        "sessions-read",
        &["mail.read".to_string()],
        chrono::Duration::days(1),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let res = bearer(
        &app,
        reqwest::Method::GET,
        "/api/v1/me/sessions",
        None,
        &secret,
        &[],
    )
    .await;
    let p = TestApp::problem(res, StatusCode::FORBIDDEN).await;
    assert_eq!(p["code"], "FORBIDDEN");
    let res = bearer(
        &app,
        reqwest::Method::PUT,
        "/api/v1/me/password",
        Some(json!({
            "current_password": TEST_PASSWORD,
            "new_password": "new-password-for-tests-2"
        })),
        &secret,
        &[("idempotency-key", "bearer-password-change-attempt")],
    )
    .await;
    let p = TestApp::problem(res, StatusCode::FORBIDDEN).await;
    assert_eq!(p["code"], "FORBIDDEN");
}

#[tokio::test]
async fn password_change_is_keyed_safe_and_revokes_other_workspaces_only() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let login = app
        .login(world.admin.email.as_deref().unwrap(), TEST_PASSWORD)
        .await;
    assert_eq!(login.status(), StatusCode::OK);
    let csrf = TestApp::csrf_token_from(&login).unwrap();
    let current_token = session_token_from(&login);
    let current_session = infrastructure::repo::auth::lookup_session(&app.state.db, &current_token)
        .await
        .unwrap()
        .unwrap()
        .session
        .id;
    let (other_session, _) =
        create_session(&app.state.db, world.workspace_id, world.admin.principal_id).await;

    // Add a second workspace principal for the same global login identity.
    let other_workspace =
        infrastructure::repo::auth::insert_workspace(&app.state.db, "Password Session Workspace")
            .await
            .unwrap();
    let mut tx = app.state.db.begin(other_workspace).await.unwrap();
    let other_principal = infrastructure::repo::auth::insert_principal(
        &mut tx,
        "human",
        world.admin.user_id,
        "Admin Elsewhere",
        "member",
        "active",
    )
    .await
    .unwrap();
    let (other_workspace_session, other_workspace_token) =
        infrastructure::repo::auth::create_session(
            &mut tx,
            other_principal,
            chrono::Duration::hours(1),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let body = json!({
        "current_password": TEST_PASSWORD,
        "new_password": "new-password-for-tests-2",
    });
    let bad = json!({
        "current_password": "wrong-current-password",
        "new_password": "new-password-for-tests-2",
    });
    let bad_res = app
        .client
        .put(app.url("/api/v1/me/password"))
        .header("x-csrf-token", &csrf)
        .header("idempotency-key", Uuid::new_v4().to_string())
        .json(&bad)
        .send()
        .await
        .unwrap();
    let p = TestApp::problem(bad_res, StatusCode::UNAUTHORIZED).await;
    assert_eq!(p["code"], "INVALID_CREDENTIALS");

    let key = format!("password-{}", Uuid::new_v4());
    let request = || {
        app.client
            .put(app.url("/api/v1/me/password"))
            .header("x-csrf-token", &csrf)
            .header("idempotency-key", &key)
            .json(&body)
    };
    let res = request().send().await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let first = res.json::<Value>().await.unwrap()["data"].take();
    assert_eq!(first, json!({"changed": true}));
    assert!(!first.to_string().contains(TEST_PASSWORD));
    assert!(!first.to_string().contains("new-password-for-tests-2"));

    // The same request replays without needing the now-stale current password.
    let res = request().send().await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.json::<Value>().await.unwrap()["data"], first);

    // This request's session survives, while every other workspace session
    // for the global user was revoked in the same transaction.
    let res = app.get("/api/v1/me").await;
    let data = TestApp::data(res).await;
    assert_eq!(data["principal_id"], world.admin.principal_id.to_string());
    let sessions = TestApp::data(app.get("/api/v1/me/sessions").await).await;
    let sessions = sessions.as_array().unwrap();
    assert!(
        sessions
            .iter()
            .any(|row| { row["id"] == current_session.to_string() && row["current"] == true })
    );
    assert!(
        sessions
            .iter()
            .any(|row| { row["id"] == other_session.to_string() && row["revoked_at"].is_string() })
    );
    let db_other = app
        .state
        .db
        .clone()
        .with_default_workspace(Some(other_workspace));
    let other_auth = infrastructure::repo::auth::lookup_session(&db_other, &other_workspace_token)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(other_auth.session.id, other_workspace_session);
    assert!(other_auth.session.revoked_at.is_some());

    let old_login = reqwest::Client::new()
        .post(app.url("/api/v1/auth/login"))
        .json(&json!({"email": world.admin.email, "password": TEST_PASSWORD}))
        .send()
        .await
        .unwrap();
    let p = TestApp::problem(old_login, StatusCode::UNAUTHORIZED).await;
    assert_eq!(p["code"], "INVALID_CREDENTIALS");
    let new_login = reqwest::Client::new()
        .post(app.url("/api/v1/auth/login"))
        .json(&json!({
            "email": world.admin.email,
            "password": "new-password-for-tests-2"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(new_login.status(), StatusCode::OK);

    // The idempotency record stores only a status body and a keyed digest of
    // the password request; neither password appears in audit metadata.
    let mut request_material = b"outreach.password-change.v1\0".to_vec();
    for password in [TEST_PASSWORD, "new-password-for-tests-2"] {
        request_material.extend_from_slice(&(password.len() as u64).to_be_bytes());
        request_material.extend_from_slice(password.as_bytes());
    }
    let expected_hash =
        infrastructure::crypto::hmac_sha256(&app.state.config.hmac_key, &request_material).to_vec();
    let scope_key =
        infrastructure::crypto::sha256_hex(world.admin.principal_id.to_string().as_bytes());
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let (request_hash, response_body): (Vec<u8>, Value) = sqlx::query_as(
        "SELECT request_hash, response_body FROM idempotency_records
          WHERE workspace_id = $1 AND scope_key = $2
            AND endpoint = 'PUT /api/v1/me/password' AND idempotency_key = $3",
    )
    .bind(world.workspace_id)
    .bind(scope_key)
    .bind(&key)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let audit: Value = sqlx::query_scalar(
        "SELECT details FROM audit_logs
          WHERE workspace_id = $1 AND action = 'user.password_changed'
          ORDER BY created_at DESC LIMIT 1",
    )
    .bind(world.workspace_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let _ = tx.rollback().await;
    assert_eq!(request_hash, expected_hash);
    assert_eq!(response_body, json!({"changed": true}));
    for password in [TEST_PASSWORD, "new-password-for-tests-2"] {
        assert!(!response_body.to_string().contains(password));
        assert!(!audit.to_string().contains(password));
    }
}

#[tokio::test]
async fn concurrent_password_changes_from_two_sessions_serialize_without_deadlock() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let client_a = reqwest::Client::builder()
        .cookie_store(true)
        .build()
        .unwrap();
    let client_b = reqwest::Client::builder()
        .cookie_store(true)
        .build()
        .unwrap();
    let login_a = client_a
        .post(app.url("/api/v1/auth/login"))
        .json(&json!({"email": world.admin.email, "password": TEST_PASSWORD}))
        .send()
        .await
        .unwrap();
    let login_b = client_b
        .post(app.url("/api/v1/auth/login"))
        .json(&json!({"email": world.admin.email, "password": TEST_PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(login_a.status(), StatusCode::OK);
    assert_eq!(login_b.status(), StatusCode::OK);
    let csrf_a = TestApp::csrf_token_from(&login_a).unwrap();
    let csrf_b = TestApp::csrf_token_from(&login_b).unwrap();

    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(3));
    let start_a = barrier.clone();
    let start_b = barrier.clone();
    let key_a = format!("parallel-a-{}", Uuid::new_v4());
    let key_b = format!("parallel-b-{}", Uuid::new_v4());
    let url_a = app.url("/api/v1/me/password");
    let url_b = app.url("/api/v1/me/password");
    let request_client_a = client_a.clone();
    let request_client_b = client_b.clone();
    let request_a = tokio::spawn(async move {
        start_a.wait().await;
        request_client_a
            .put(url_a)
            .header("x-csrf-token", csrf_a)
            .header("idempotency-key", key_a)
            .json(&json!({
                "current_password": TEST_PASSWORD,
                "new_password": "concurrent-password-a",
            }))
            .send()
            .await
            .unwrap()
    });
    let request_b = tokio::spawn(async move {
        start_b.wait().await;
        request_client_b
            .put(url_b)
            .header("x-csrf-token", csrf_b)
            .header("idempotency-key", key_b)
            .json(&json!({
                "current_password": TEST_PASSWORD,
                "new_password": "concurrent-password-b",
            }))
            .send()
            .await
            .unwrap()
    });
    barrier.wait().await;
    let (res_a, res_b) = tokio::join!(request_a, request_b);
    let res_a = res_a.unwrap();
    let res_b = res_b.unwrap();
    let a_succeeded = res_a.status() == StatusCode::OK;
    let b_succeeded = res_b.status() == StatusCode::OK;
    assert_ne!(a_succeeded, b_succeeded, "exactly one password change wins");

    let loser = if a_succeeded { res_b } else { res_a };
    let p = TestApp::problem(loser, StatusCode::UNAUTHORIZED).await;
    assert!(
        p["code"] == "AUTHENTICATION_REQUIRED" || p["code"] == "INVALID_CREDENTIALS",
        "losing session is denied without a database error: {p}"
    );

    let hash = infrastructure::repo::auth::get_password_credential(
        &app.state.db,
        world.admin.user_id.unwrap(),
    )
    .await
    .unwrap()
    .unwrap()
    .password_hash;
    assert_ne!(
        infrastructure::crypto::verify_password("concurrent-password-a", &hash),
        infrastructure::crypto::verify_password("concurrent-password-b", &hash),
        "exactly one new password must be committed"
    );

    let (winner, losing_client) = if a_succeeded {
        (&client_a, &client_b)
    } else {
        (&client_b, &client_a)
    };
    assert_eq!(
        winner
            .get(app.url("/api/v1/me"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        losing_client
            .get(app.url("/api/v1/me"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
}

// --------------------------------------------------------------------------
// A.7 last-active-admin guard
// --------------------------------------------------------------------------

#[tokio::test]
async fn last_active_admin_cannot_be_disabled_or_demoted() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let csrf = admin_csrf(&app, &world).await;
    let path = format!("/api/v1/principals/{}", world.admin.principal_id);

    // Demote the only admin → 409.
    let res = app.get(&path).await;
    let etag = res
        .headers()
        .get("etag")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let res = patch(&app, &path, json!({"role": "member"}), &csrf, Some(&etag)).await;
    let p = TestApp::problem(res, StatusCode::CONFLICT).await;
    assert_eq!(p["code"], "CONFLICT");

    // Disable-request on the only admin → 409 too.
    let res = post_idem(
        &app,
        &format!("{path}/disable-requests"),
        json!({}),
        &csrf,
        Some(&Uuid::new_v4().to_string()),
    )
    .await;
    let p = TestApp::problem(res, StatusCode::CONFLICT).await;
    assert_eq!(p["code"], "CONFLICT");
}

#[tokio::test]
async fn pending_admin_disable_reserves_the_last_active_admin_slot() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let csrf = admin_csrf(&app, &world).await;

    // Promote the manager so the workspace has two active administrators.
    let manager_path = format!("/api/v1/principals/{}", world.manager.principal_id);
    let res = app.get(&manager_path).await;
    let etag = res
        .headers()
        .get("etag")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let res = patch(
        &app,
        &manager_path,
        json!({"role": "admin"}),
        &csrf,
        Some(&etag),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);

    // Queue cleanup for the second admin. Their login is revoked now, while
    // their principal remains active until the worker completes cleanup.
    let manager_disable = format!(
        "/api/v1/principals/{}/disable-requests",
        world.manager.principal_id
    );
    let res = post_idem(
        &app,
        &manager_disable,
        json!({"reassign_to": world.member_a.principal_id}),
        &csrf,
        Some(&Uuid::new_v4().to_string()),
    )
    .await;
    assert_eq!(res.status(), StatusCode::ACCEPTED);

    // The only administrator not already pending deactivation cannot also
    // be disabled, or a worker could leave the workspace with no admin.
    let admin_disable = format!(
        "/api/v1/principals/{}/disable-requests",
        world.admin.principal_id
    );
    let res = post_idem(
        &app,
        &admin_disable,
        json!({"reassign_to": world.member_a.principal_id}),
        &csrf,
        Some(&Uuid::new_v4().to_string()),
    )
    .await;
    let p = TestApp::problem(res, StatusCode::CONFLICT).await;
    assert_eq!(p["code"], "CONFLICT");
}

// --------------------------------------------------------------------------
// member invitations — one-time link + revoke
// --------------------------------------------------------------------------

#[tokio::test]
async fn invitation_link_returned_once_and_revocable() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let csrf = admin_csrf(&app, &world).await;
    let key = format!("inv-{}", Uuid::new_v4());
    let email = format!(
        "newbie-{}@test.dev",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let body = json!({"email": email, "role": "member"});

    // Fresh call: invite_url present exactly once.
    let res = post_idem(
        &app,
        "/api/v1/member-invitations",
        body.clone(),
        &csrf,
        Some(&key),
    )
    .await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let data = res.json::<Value>().await.unwrap()["data"].take();
    let inv_id = data["id"].as_str().unwrap().to_string();
    let url = data["invite_url"]
        .as_str()
        .expect("one-time invite_url missing");
    assert!(url.contains("/api/v1/auth/invitations/"), "bad link {url}");

    // Replay with the same key + same body → stored response WITHOUT the
    // link (the token is never stored nor re-emitted).
    let res = post_idem(
        &app,
        "/api/v1/member-invitations",
        body.clone(),
        &csrf,
        Some(&key),
    )
    .await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let data = res.json::<Value>().await.unwrap()["data"].take();
    assert_eq!(data["id"], inv_id, "replay must return the same invitation");
    assert!(
        data["invite_url"].is_null(),
        "invite_url must not replay: {data}"
    );

    // Same key + different body → 409 IDEMPOTENCY_KEY_REUSED.
    let res = post_idem(
        &app,
        "/api/v1/member-invitations",
        json!({"email": format!("other-{email}"), "role": "member"}),
        &csrf,
        Some(&key),
    )
    .await;
    let p = TestApp::problem(res, StatusCode::CONFLICT).await;
    assert_eq!(p["code"], "IDEMPOTENCY_KEY_REUSED");

    // List shows the invitation (email+role, never the token).
    let res = app.get("/api/v1/member-invitations").await;
    let data = TestApp::data(res).await;
    let inv = data
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == inv_id)
        .expect("invitation not listed");
    assert_eq!(inv["email"], email);
    assert!(inv.get("invite_url").is_none() || inv["invite_url"].is_null());
    assert!(inv["revoked_at"].is_null());

    // Revoke → revoked_at set; second revoke stays 200 (idempotent).
    let res = app
        .post_with_csrf(
            &format!("/api/v1/member-invitations/{inv_id}/revoke"),
            json!({}),
            &csrf,
        )
        .await;
    let data = TestApp::data(res).await;
    assert!(data["revoked_at"].is_string());
    let res = app
        .post_with_csrf(
            &format!("/api/v1/member-invitations/{inv_id}/revoke"),
            json!({}),
            &csrf,
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK);
}

// --------------------------------------------------------------------------
// agents + service credentials — secret once, hashed at rest
// --------------------------------------------------------------------------

#[tokio::test]
async fn agent_and_credential_lifecycle() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let csrf = admin_csrf(&app, &world).await;

    // Unknown agent scopes are rejected at creation.
    let res = app
        .post_with_csrf(
            "/api/v1/agents",
            json!({"name": "Bad Agent", "scopes": ["not.a_scope"]}),
            &csrf,
        )
        .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // Agent/human parity: send/approve scopes are valid for agents.
    let res = app
        .post_with_csrf(
            "/api/v1/agents",
            json!({"name": "Sender Agent", "scopes": ["mail.send", "mail.approve"]}),
            &csrf,
        )
        .await;
    assert_eq!(res.status(), StatusCode::CREATED);

    // Create agent.
    let res = app
        .post_with_csrf(
            "/api/v1/agents",
            json!({"name": "Api Agent", "scopes": ["mail.read", "agents.run"]}),
            &csrf,
        )
        .await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let data = res.json::<Value>().await.unwrap()["data"].take();
    assert_eq!(data["kind"], "agent");
    let agent_id = data["id"].as_str().unwrap().to_string();

    // Issue a credential — `secret` returns exactly once.
    let key = format!("cred-{}", Uuid::new_v4());
    let cred_body = json!({"name": "agent-key", "scopes": ["mail.read"]});
    let res = post_idem(
        &app,
        &format!("/api/v1/agents/{agent_id}/credentials"),
        cred_body.clone(),
        &csrf,
        Some(&key),
    )
    .await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let data = res.json::<Value>().await.unwrap()["data"].take();
    let secret = data["secret"].as_str().expect("one-time secret missing");
    assert!(secret.starts_with("osk_"), "bad prefix: {secret}");
    let cred_id = data["id"].as_str().unwrap().to_string();

    // A never-expiring credential is issued with a far-future expiry and works.
    let res = post_idem(
        &app,
        &format!("/api/v1/agents/{agent_id}/credentials"),
        json!({"name": "forever-key", "scopes": ["mail.read"], "never_expires": true}),
        &csrf,
        Some(&format!("cred-forever-{}", Uuid::new_v4())),
    )
    .await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let forever = res.json::<Value>().await.unwrap()["data"].take();
    let expires = chrono::DateTime::parse_from_rfc3339(forever["expires_at"].as_str().unwrap())
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert!(expires > chrono::Utc::now() + chrono::Duration::days(365 * 50));
    let forever_secret = forever["secret"].as_str().unwrap().to_string();
    let res = bearer(
        &app,
        reqwest::Method::GET,
        "/api/v1/me",
        None,
        &forever_secret,
        &[],
    )
    .await;
    assert_eq!(TestApp::data(res).await["principal_id"], agent_id);

    // Replay: same id, no secret.
    let res = post_idem(
        &app,
        &format!("/api/v1/agents/{agent_id}/credentials"),
        cred_body,
        &csrf,
        Some(&key),
    )
    .await;
    let data = res.json::<Value>().await.unwrap()["data"].take();
    assert_eq!(data["id"], cred_id);
    assert!(data["secret"].is_null(), "secret must not replay");

    // The DB row carries only prefix + sha256 (token_hash is never
    // selected by the repo row; the plaintext-vs-hash assertion lives in
    // the infrastructure test where sqlx is available).
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let row =
        infrastructure::repo::org::get_credential(&mut tx, Uuid::parse_str(&cred_id).unwrap())
            .await
            .unwrap()
            .expect("credential missing");
    let _ = tx.rollback().await;
    assert!(secret.starts_with(&row.key_prefix), "key_prefix must match");

    // Bearer works.
    let res = bearer(&app, reqwest::Method::GET, "/api/v1/me", None, secret, &[]).await;
    let data = TestApp::data(res).await;
    assert_eq!(data["principal_id"], agent_id);

    // List shows the credential (without secrets).
    let res = app.get("/api/v1/credentials").await;
    let data = TestApp::data(res).await;
    let cred = data
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == cred_id)
        .expect("credential not listed");
    assert!(cred.get("secret").is_none() || cred["secret"].is_null());
    assert_eq!(cred["scopes"], json!(["mail.read"]));

    // Revoke → enqueues org.revoke_credential_jobs and kills the token.
    let res = app
        .post_with_csrf(
            &format!("/api/v1/credentials/{cred_id}/revoke"),
            json!({}),
            &csrf,
        )
        .await;
    let data = TestApp::data(res).await;
    assert!(data["revoked_at"].is_string());
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let job = infrastructure::repo::org::find_job_by_dedupe(
        &mut tx,
        "org.revoke_credential_jobs",
        &format!("revoke-cred:{cred_id}"),
    )
    .await
    .unwrap();
    let _ = tx.rollback().await;
    assert!(job.is_some(), "revoke job must be enqueued");

    let res = bearer(&app, reqwest::Method::GET, "/api/v1/me", None, secret, &[]).await;
    let p = TestApp::problem(res, StatusCode::UNAUTHORIZED).await;
    assert_eq!(p["code"], "AUTHENTICATION_REQUIRED");
}

// --------------------------------------------------------------------------
// disable-requests — revoke access immediately, but deactivate after cleanup
// --------------------------------------------------------------------------

#[tokio::test]
async fn disable_request_queues_cleanup_and_revokes_access_before_deactivation() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let csrf = admin_csrf(&app, &world).await;

    // member_b has a live session.
    let member_client = reqwest::Client::builder()
        .cookie_store(true)
        .build()
        .unwrap();
    let res = member_client
        .post(app.url("/api/v1/auth/login"))
        .json(&json!({"email": world.member_b.email, "password": TEST_PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let path = format!(
        "/api/v1/principals/{}/disable-requests",
        world.member_b.principal_id
    );
    let key = format!("dis-{}", Uuid::new_v4());
    let res = post_idem(
        &app,
        &path,
        json!({"reassign_to": world.manager.principal_id, "cancel_pending": true}),
        &csrf,
        Some(&key),
    )
    .await;
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    let data = res.json::<Value>().await.unwrap()["data"].take();
    let job_id = data["job_id"].as_str().unwrap().to_string();

    // Idempotent replay returns the same job id.
    let res = post_idem(
        &app,
        &path,
        json!({"reassign_to": world.manager.principal_id, "cancel_pending": true}),
        &csrf,
        Some(&key),
    )
    .await;
    let data = res.json::<Value>().await.unwrap()["data"].take();
    assert_eq!(data["job_id"], job_id);

    // Sessions and future password login are revoked immediately. The
    // principal remains active until the cleanup worker atomically cancels
    // pending work, reassigns owned rows, and commits the final transition.
    let res = member_client
        .get(app.url("/api/v1/me"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let res = member_client
        .post(app.url("/api/v1/auth/login"))
        .json(&json!({"email": world.member_b.email, "password": TEST_PASSWORD}))
        .send()
        .await
        .unwrap();
    let p = TestApp::problem(res, StatusCode::UNAUTHORIZED).await;
    assert_eq!(p["code"], "INVALID_CREDENTIALS");

    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let principal =
        infrastructure::repo::org::get_principal_full(&mut tx, world.member_b.principal_id)
            .await
            .unwrap()
            .expect("principal missing");
    let job_found =
        infrastructure::repo::org::pending_disable_job(&mut tx, world.member_b.principal_id)
            .await
            .unwrap();
    let _ = tx.rollback().await;
    assert_eq!(principal.status, "active");
    assert_eq!(job_found.unwrap().to_string(), job_id);
    assert!(
        !outbox_has(
            &app.state.db,
            world.workspace_id,
            "principal.disabled",
            world.member_b.principal_id,
        )
        .await,
        "principal.disabled must wait until cleanup commits"
    );

    // A second request for an in-flight disable attaches to the same job.
    let res = post_idem(
        &app,
        &path,
        json!({}),
        &csrf,
        Some(&Uuid::new_v4().to_string()),
    )
    .await;
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    let data = res.json::<Value>().await.unwrap()["data"].take();
    assert_eq!(data["job_id"], job_id);

    // Service credentials are revoked in the request transaction as well,
    // so an Agent cannot regain access while its principal cleanup is queued.
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let (_credential_id, agent_secret) = infrastructure::repo::auth::create_service_credential(
        &mut tx,
        world.agent.principal_id,
        "pending-disable-test",
        &["mail.read".to_string()],
        chrono::Duration::days(1),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let res = post_idem(
        &app,
        &format!(
            "/api/v1/principals/{}/disable-requests",
            world.agent.principal_id
        ),
        json!({}),
        &csrf,
        Some(&Uuid::new_v4().to_string()),
    )
    .await;
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    let res = bearer(
        &app,
        reqwest::Method::GET,
        "/api/v1/me",
        None,
        &agent_secret,
        &[],
    )
    .await;
    let p = TestApp::problem(res, StatusCode::UNAUTHORIZED).await;
    assert_eq!(p["code"], "AUTHENTICATION_REQUIRED");

    // Missing Idempotency-Key → 400.
    let res = post_idem(
        &app,
        &format!(
            "/api/v1/principals/{}/disable-requests",
            world.member_a.principal_id
        ),
        json!({}),
        &csrf,
        None,
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

// --------------------------------------------------------------------------
// emergency stop (B.2 운영)
// --------------------------------------------------------------------------

#[tokio::test]
async fn emergency_stop_requires_roles_reason_and_version() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    // NOTE: the test client's cookie jar holds the LAST login — keep each
    // principal's requests contiguous with their own CSRF token.
    let admin_csrf = admin_csrf(&app, &world).await;

    // Default ON (DDL default), ETag = policy_version.
    let res = app.get("/api/v1/operations/emergency-stop").await;
    let etag = res
        .headers()
        .get("etag")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let data = TestApp::data(res).await;
    assert_eq!(data["enabled"], true);
    let v1 = data["policy_version"].as_i64().unwrap();
    assert_eq!(etag, format!("\"{v1}\""));

    // OFF without a reason → 400 (authz passes for admin, then reason).
    let res = put(
        &app,
        "/api/v1/operations/emergency-stop",
        json!({"enabled": false, "reason": "  "}),
        &admin_csrf,
        &[("if-match", &etag)],
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // Missing If-Match → 428.
    let res = put(
        &app,
        "/api/v1/operations/emergency-stop",
        json!({"enabled": false, "reason": "maintenance done"}),
        &admin_csrf,
        &[],
    )
    .await;
    let p = TestApp::problem(res, StatusCode::PRECONDITION_REQUIRED).await;
    assert_eq!(p["code"], "VERSION_PRECONDITION_REQUIRED");

    // Admin OFF with reason + If-Match → 200, version advances.
    let res = put(
        &app,
        "/api/v1/operations/emergency-stop",
        json!({"enabled": false, "reason": "maintenance done"}),
        &admin_csrf,
        &[("if-match", &etag)],
    )
    .await;
    let etag2 = res
        .headers()
        .get("etag")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let data = TestApp::data(res).await;
    assert_eq!(data["enabled"], false);
    assert_ne!(etag, etag2);

    // Stale version → 412.
    let res = put(
        &app,
        "/api/v1/operations/emergency-stop",
        json!({"enabled": true, "reason": ""}),
        &admin_csrf,
        &[("if-match", &etag)],
    )
    .await;
    let p = TestApp::problem(res, StatusCode::PRECONDITION_FAILED).await;
    assert_eq!(p["code"], "VERSION_MISMATCH");

    // Manager: may turn it back ON, may NOT turn it OFF.
    let manager_csrf = login_csrf(&app, &world.manager).await;
    let res = put(
        &app,
        "/api/v1/operations/emergency-stop",
        json!({"enabled": false, "reason": "maintenance done"}),
        &manager_csrf,
        &[("if-match", &etag2)],
    )
    .await;
    let p = TestApp::problem(res, StatusCode::FORBIDDEN).await;
    assert_eq!(p["code"], "FORBIDDEN");
    let res = put(
        &app,
        "/api/v1/operations/emergency-stop",
        json!({"enabled": true, "reason": "resume freeze"}),
        &manager_csrf,
        &[("if-match", &etag2)],
    )
    .await;
    let data = TestApp::data(res).await;
    assert_eq!(data["enabled"], true);
    let etag3 = format!("\"{}\"", data["policy_version"].as_i64().unwrap());

    // Member cannot touch it at all (ON is admin/manager; OFF is admin).
    let member_csrf = login_csrf(&app, &world.member_a).await;
    let res = put(
        &app,
        "/api/v1/operations/emergency-stop",
        json!({"enabled": true, "reason": ""}),
        &member_csrf,
        &[("if-match", &etag3)],
    )
    .await;
    let p = TestApp::problem(res, StatusCode::FORBIDDEN).await;
    assert_eq!(p["code"], "FORBIDDEN");

    // Both transitions audited.
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let n = infrastructure::repo::org::audit_action_count(&mut tx, "operations.emergency_stop")
        .await
        .unwrap();
    let _ = tx.rollback().await;
    assert!(n >= 2, "expected audit rows for stop + resume, got {n}");
}

// --------------------------------------------------------------------------
// T03 — agent/human parity: agents administer like an Admin; audited
// --------------------------------------------------------------------------

#[tokio::test]
async fn t03_agent_admin_endpoints_allowed_and_audited() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let csrf = admin_csrf(&app, &world).await;

    // Fresh agent + credential carrying org.admin (Admin-equivalent).
    let res = app
        .post_with_csrf(
            "/api/v1/agents",
            json!({"name": "T03 Agent", "scopes": ["mail.read", "org.admin"]}),
            &csrf,
        )
        .await;
    let agent_id = res.json::<Value>().await.unwrap()["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let res = app
        .post_with_csrf(
            &format!("/api/v1/agents/{agent_id}/credentials"),
            json!({"name": "k", "scopes": ["mail.read", "org.admin"]}),
            &csrf,
        )
        .await;
    let secret = res.json::<Value>().await.unwrap()["data"]["secret"]
        .as_str()
        .unwrap()
        .to_string();
    let agent_uuid = Uuid::parse_str(&agent_id).unwrap();

    // 1) grant a mailbox (allowed: agents hold Admin-equivalent authority)
    let res = bearer(
        &app,
        reqwest::Method::PUT,
        &format!(
            "/api/v1/mail-accounts/{}/grants/{}",
            world.mail_account_id, world.member_b.principal_id
        ),
        Some(json!({
            "can_read": true, "can_compose": false, "can_send": false,
            "can_approve": false, "can_assign": false
        })),
        &secret,
        &[("if-none-match", "*")],
    )
    .await;
    let data = TestApp::data(res).await;
    assert_eq!(data["can_read"], true);

    // 2) change a role (allowed)
    let res = bearer(
        &app,
        reqwest::Method::PATCH,
        &format!("/api/v1/principals/{}", world.member_b.principal_id),
        Some(json!({"role": "manager"})),
        &secret,
        &[("if-match", "\"1\"")],
    )
    .await;
    let data = TestApp::data(res).await;
    assert_eq!(data["role"], "manager");

    // 3) request a disable (allowed)
    let res = bearer(
        &app,
        reqwest::Method::POST,
        &format!(
            "/api/v1/principals/{}/disable-requests",
            world.member_b.principal_id
        ),
        Some(json!({})),
        &secret,
        &[("idempotency-key", &Uuid::new_v4().to_string())],
    )
    .await;
    assert!(
        res.status().is_success(),
        "agent disable request failed: {}",
        res.status()
    );

    // 4) create another agent (allowed)
    let res = bearer(
        &app,
        reqwest::Method::POST,
        "/api/v1/agents",
        Some(json!({"name": "spawn"})),
        &secret,
        &[],
    )
    .await;
    assert_eq!(res.status(), StatusCode::CREATED);

    // No denials; every action still wrote an audit row.
    for action in [
        "mailbox.grant",
        "principal.update",
        "principal.disable",
        "agent.create",
    ] {
        let n = denied_audits(&app.state.db, world.workspace_id, agent_uuid, action).await;
        assert_eq!(n, 0, "unexpected denied audit for {action}");
        let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
        let total = infrastructure::repo::org::audit_action_count(&mut tx, action)
            .await
            .unwrap();
        let _ = tx.rollback().await;
        assert!(total >= 1, "missing audit row for {action}");
    }
}
