//! Fix-round-1 HTTP coverage: R4 concurrent invitation accept, R6 client-IP
//! trust rules, R9 protective headers, R10 uniform INVALID_CREDENTIALS,
//! R26 PageParams validation.

use std::sync::Arc;

use api::AppState;
use api::testing::TestApp;
use axum::extract::FromRequestParts;
use infrastructure::config::Config;
use infrastructure::db::{Db, ROLE_API};
use infrastructure::repo::auth as auth_repo;
use infrastructure::testing::{TEST_PASSWORD, TestWorld, seed_workspace};
use reqwest::StatusCode;
use serde_json::json;

async fn test_app() -> (TestApp, TestWorld) {
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
    (app, world)
}

/// R4: two concurrent accepts of the same invitation → exactly one session.
#[tokio::test]
async fn concurrent_invitation_accept_single_session() {
    let (app, world) = test_app().await;

    let (_inv_id, token) = {
        let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
        let r = auth_repo::create_invitation(
            &mut tx,
            "race@example.com",
            "member",
            world.admin.principal_id,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        r
    };

    let url = app.url(&format!("/api/v1/auth/invitations/{token}/accept"));
    let body = json!({"display_name": "Race", "password": "ValidPassw0rd!"});
    let c = app.client.clone();
    let (r1, r2) = tokio::join!(
        c.post(&url).json(&body).send(),
        c.post(&url).json(&body).send()
    );
    let (s1, s2) = (r1.unwrap().status(), r2.unwrap().status());
    let statuses = [s1, s2];
    assert!(
        statuses.contains(&StatusCode::OK),
        "one accept must succeed: {statuses:?}"
    );
    let losers = statuses.iter().filter(|s| **s != StatusCode::OK).count();
    assert_eq!(losers, 1, "the losing accept must fail: {statuses:?}");
}

/// R6: `X-Forwarded-For` is honored only when the TCP peer is a trusted
/// proxy; the rightmost untrusted entry wins.
#[test]
fn client_ip_trusts_only_trusted_proxies() {
    use std::net::IpAddr;
    let config = Config::test(); // trusted: 127.0.0.1/32, ::1/128
    let peer: IpAddr = "127.0.0.1".parse().unwrap();
    let untrusted_peer: IpAddr = "203.0.113.9".parse().unwrap();

    // Untrusted peer → XFF ignored entirely.
    let mut h = axum::http::HeaderMap::new();
    h.insert("x-forwarded-for", "1.2.3.4".parse().unwrap());
    assert_eq!(
        api::extract::client_ip(&config, &h, untrusted_peer),
        untrusted_peer
    );

    // Trusted peer → rightmost untrusted XFF entry wins.
    let mut h = axum::http::HeaderMap::new();
    h.insert("x-forwarded-for", "1.2.3.4, 127.0.0.1".parse().unwrap());
    assert_eq!(
        api::extract::client_ip(&config, &h, peer),
        "1.2.3.4".parse::<IpAddr>().unwrap()
    );

    // Trusted peer, no XFF → peer itself.
    let h = axum::http::HeaderMap::new();
    assert_eq!(api::extract::client_ip(&config, &h, peer), peer);
}

/// R9: auth/public/PII responses carry the protective header set.
#[tokio::test]
async fn protective_headers_on_auth_and_me() {
    let (app, world) = test_app().await;

    // Auth route — full header set.
    let res = app
        .client
        .post(app.url("/api/v1/auth/login"))
        .json(&json!({"email": "nobody@x.dev", "password": "x"}))
        .send()
        .await
        .unwrap();
    let h = res.headers();
    assert_eq!(h["cache-control"], "no-store");
    assert_eq!(h["referrer-policy"], "no-referrer");
    assert_eq!(h["x-robots-tag"], "noindex, nofollow");

    // /me — PII route.
    let email = world.admin.email.as_deref().unwrap();
    let res = app.login(email, TEST_PASSWORD).await;
    assert_eq!(res.status(), StatusCode::OK);
    let res = app.get("/api/v1/me").await;
    let h = res.headers().clone();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(h["cache-control"], "no-store");
    assert_eq!(h["referrer-policy"], "no-referrer");
    assert_eq!(h["x-robots-tag"], "noindex, nofollow");
}

/// R10: unknown email and wrong password produce the identical problem
/// shape — no user enumeration signal.
#[tokio::test]
async fn uniform_invalid_credentials() {
    let (app, world) = test_app().await;
    let email = world.member_a.email.as_deref().unwrap();

    let res = app.login("ghost@nowhere.dev", "whatever-pass1").await;
    let unknown = TestApp::problem(res, StatusCode::UNAUTHORIZED).await;
    let res = app.login(email, "wrong-password!!").await;
    let wrong = TestApp::problem(res, StatusCode::UNAUTHORIZED).await;

    assert_eq!(unknown["code"], "INVALID_CREDENTIALS");
    assert_eq!(wrong["code"], "INVALID_CREDENTIALS");
    assert_eq!(unknown["title"], wrong["title"]);
    assert_eq!(unknown["detail"], wrong["detail"]);
    // Identical member set — no extra signal.
    let keys = |v: &serde_json::Value| {
        let mut k: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        k.sort();
        k
    };
    assert_eq!(keys(&unknown), keys(&wrong));
}

/// R26: `PageParams` — default 50, clamp 200, 0/negative/non-numeric → 400.
#[tokio::test]
async fn page_params_validation() {
    let (app, _world) = test_app().await;
    let state = app.state.clone();

    async fn params(
        state: &AppState,
        uri: &str,
    ) -> Result<api::extract::PageParams, api::problem::ProblemError> {
        let req = axum::http::Request::builder().uri(uri).body(()).unwrap();
        let mut parts = req.into_parts().0;
        api::extract::PageParams::from_request_parts(&mut parts, state).await
    }

    let p = params(&state, "/x").await.unwrap();
    assert_eq!(p.limit, 50);
    assert!(p.cursor.is_none());

    let p = params(&state, "/x?limit=10").await.unwrap();
    assert_eq!(p.limit, 10);
    let p = params(&state, "/x?limit=999").await.unwrap();
    assert_eq!(p.limit, 200, "clamped to max");
    let p = params(&state, "/x?limit=10&cursor=abc.def").await.unwrap();
    assert_eq!(p.cursor.as_deref(), Some("abc.def"));

    for uri in ["/x?limit=0", "/x?limit=-3", "/x?limit=abc"] {
        let err = params(&state, uri).await.unwrap_err();
        assert_eq!(err.status(), 400, "{uri}");
        assert_eq!(err.code, contracts::ErrorCode::ValidationFailed);
    }
}
