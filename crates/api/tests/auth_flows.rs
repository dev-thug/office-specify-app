//! Task 1 step-9 HTTP tests: `/me` unauthenticated, login cookies, account
//! lockout after 5 failures, CSRF rejection on session mutations.

use api::AppState;
use api::testing::TestApp;
use infrastructure::config::Config;
use infrastructure::db::{Db, ROLE_API};
use infrastructure::testing::{TEST_PASSWORD, seed_workspace};
use reqwest::StatusCode;
use serde_json::json;
use std::sync::Arc;

async fn test_app() -> Option<(TestApp, infrastructure::testing::TestWorld)> {
    // DB tests must never silently pass — a missing DATABASE_URL is a
    // failure, not a skip.
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for DB tests");
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

#[tokio::test]
async fn me_requires_auth() {
    let Some((app, _)) = test_app().await else {
        return;
    };
    let res = app.get("/api/v1/me").await;
    let problem = TestApp::problem(res, StatusCode::UNAUTHORIZED).await;
    assert_eq!(problem["code"], "AUTHENTICATION_REQUIRED");
}

#[tokio::test]
async fn login_sets_cookies_and_me_returns_principal() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let email = world.admin.email.as_deref().unwrap();
    let res = app.login(email, TEST_PASSWORD).await;
    assert_eq!(res.status(), StatusCode::OK, "login failed");

    // Both cookies set; CSRF token present for the double-submit check.
    let cookies: Vec<_> = res
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok().map(String::from))
        .collect();
    assert!(cookies.iter().any(|c| c.starts_with("outreach_session=")));
    assert!(cookies.iter().any(|c| c.starts_with("outreach_csrf=")));
    assert!(
        cookies.iter().all(|c| c.contains("HttpOnly")
            || c.contains("httponly")
            || c.starts_with("outreach_csrf=")),
        "session cookie must be HttpOnly: {cookies:?}"
    );
    assert!(TestApp::csrf_token_from(&res).is_some());

    let res = app.get("/api/v1/me").await;
    let data = TestApp::data(res).await;
    assert_eq!(data["principal_id"], world.admin.principal_id.to_string());
    assert_eq!(data["role"], "admin");
}

#[tokio::test]
async fn login_lockout_after_five_failures() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let email = world.member_a.email.as_deref().unwrap();

    for i in 0..4 {
        let res = app.login(email, "wrong-password!!").await;
        let p = TestApp::problem(res, StatusCode::UNAUTHORIZED).await;
        assert_eq!(p["code"], "INVALID_CREDENTIALS", "attempt {i}");
    }
    // Fifth failure locks the account.
    let res = app.login(email, "wrong-password!!").await;
    let p = TestApp::problem(res, StatusCode::TOO_MANY_REQUESTS).await;
    assert_eq!(p["code"], "ACCOUNT_LOCKED");

    // Correct password is still rejected while locked.
    let res = app.login(email, TEST_PASSWORD).await;
    let p = TestApp::problem(res, StatusCode::TOO_MANY_REQUESTS).await;
    assert_eq!(p["code"], "ACCOUNT_LOCKED");
}

#[tokio::test]
async fn session_mutation_without_csrf_is_rejected() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let email = world.manager.email.as_deref().unwrap();
    let res = app.login(email, TEST_PASSWORD).await;
    let csrf = TestApp::csrf_token_from(&res).unwrap();

    // POST with the session cookie but no X-CSRF-Token → 403.
    let res = app.post_no_csrf("/api/v1/auth/logout", json!({})).await;
    let p = TestApp::problem(res, StatusCode::FORBIDDEN).await;
    assert_eq!(p["code"], "CSRF_TOKEN_INVALID");

    // Mismatched token → also rejected.
    let res = app
        .post_with_csrf("/api/v1/auth/logout", json!({}), "bogus")
        .await;
    let p = TestApp::problem(res, StatusCode::FORBIDDEN).await;
    assert_eq!(p["code"], "CSRF_TOKEN_INVALID");

    // Correct double-submit succeeds (204).
    let res = app
        .post_with_csrf("/api/v1/auth/logout", json!({}), &csrf)
        .await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    // Session is now revoked — /me is 401 again.
    let res = app.get("/api/v1/me").await;
    let p = TestApp::problem(res, StatusCode::UNAUTHORIZED).await;
    assert_eq!(p["code"], "AUTHENTICATION_REQUIRED");
}
