//! Compliance HTTP tests (spec §11, B.2, B.4.4, B.5):
//! - internal `/api/v1/communication-bases` (+reviews, +revoke) and
//!   `/suppressions` (+release-requests)
//! - public `/public/v1/unsubscribe/{token}` GET/POST/one-click with no
//!   session — the URL token is the only capability.

use api::AppState;
use api::testing::TestApp;
use chrono::{Duration, Utc};
use infrastructure::config::Config;
use infrastructure::db::{Db, ROLE_API, Tx};
use infrastructure::jobs;
use infrastructure::repo::approvals::{self as approval_repo, ApprovalTarget};
use infrastructure::repo::compliance::test_fixtures as fixtures;
use infrastructure::repo::compliance_core;
use infrastructure::testing::{TEST_PASSWORD, TestWorld, seed_workspace};
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

async fn test_app() -> Option<(TestApp, TestWorld)> {
    test_app_with(None).await
}

/// App + seeded world; `limiter` wires a real Redis-backed rate limiter
/// (agent suppression quota test). Pass `None` to run without one.
async fn test_app_with(
    limiter: Option<infrastructure::ratelimit::RateLimiter>,
) -> Option<(TestApp, TestWorld)> {
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
    // Public token lookups run in a pre-auth tx under the server-fixed
    // default workspace (A.5).
    let db = db.with_default_workspace(Some(world.workspace_id));
    let app = TestApp::spawn_with_state(AppState::with_fake_providers(
        db,
        Arc::new(Config::test()),
        limiter,
    ))
    .await;
    Some((app, world))
}

/// Redis limiter for the agent-quota test — skipped when unreachable.
async fn redis_limiter() -> Option<infrastructure::ratelimit::RateLimiter> {
    let url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".into());
    match infrastructure::redis::RedisClient::connect(&url).await {
        Ok(r) => Some(infrastructure::ratelimit::RateLimiter::new(r)),
        Err(e) => {
            eprintln!("Redis unreachable ({e}) — skipping rate-limit test");
            None
        }
    }
}

/// Admin session: login, capture the CSRF cookie for mutations.
async fn admin_session(app: &TestApp, world: &TestWorld) -> String {
    let res = app
        .login(world.admin.email.as_deref().unwrap(), TEST_PASSWORD)
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    TestApp::csrf_token_from(&res).expect("csrf cookie")
}

async fn insert_email_address(tx: &mut Tx<'_>, local: &str) -> Uuid {
    fixtures::insert_email_address(tx, &format!("{local}@example.com"))
        .await
        .unwrap()
}

async fn insert_outbound(tx: &mut Tx<'_>, world: &TestWorld, to: Uuid) -> Uuid {
    fixtures::insert_outbound(
        tx,
        world.mail_account_id,
        world.connection_id,
        world.admin.principal_id,
        to,
    )
    .await
    .unwrap()
}

/// Issue a real unsubscribe token via the shared core.
async fn issue_token(app: &TestApp, world: &TestWorld, local: &str) -> String {
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let addr = insert_email_address(&mut tx, local).await;
    let outbound = insert_outbound(&mut tx, world, addr).await;
    let raw = compliance_core::issue_unsubscribe_token(&mut tx, addr, outbound)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    raw.token
}

/// Direct DB view of a token row (verification without trusting the API).
async fn token_used_at(
    app: &TestApp,
    ws: Uuid,
    token: &str,
) -> Option<Option<chrono::DateTime<chrono::Utc>>> {
    let mut tx = app.state.db.begin(ws).await.unwrap();
    let row = fixtures::token_used_at(&mut tx, token).await.unwrap();
    tx.rollback().await.unwrap();
    row
}

async fn suppression_count(app: &TestApp, ws: Uuid) -> i64 {
    let mut tx = app.state.db.begin(ws).await.unwrap();
    let n = fixtures::suppression_count(&mut tx).await.unwrap();
    tx.rollback().await.unwrap();
    n
}

fn assert_public_headers(res: &reqwest::Response) {
    assert_eq!(
        res.headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok()),
        Some("no-store")
    );
    assert_eq!(
        res.headers()
            .get("referrer-policy")
            .and_then(|v| v.to_str().ok()),
        Some("no-referrer")
    );
    assert!(
        res.headers()
            .get("x-robots-tag")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|value| value
                .split(',')
                .any(|directive| directive.trim() == "noindex"))
    );
}

/// T23 — GET and HEAD on the unsubscribe link never change state (mail
/// scanners prefetch links). The confirmation GET is pure.
#[tokio::test]
async fn t23_get_and_head_unsubscribe_no_state_change() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let token = issue_token(&app, &world, "t23").await;
    let url = app.url(&format!("/public/v1/unsubscribe/{token}"));

    // GET → 200 confirmation data, zero writes.
    let res = app.client.get(&url).send().await.unwrap();
    assert_public_headers(&res);
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["data"]["status"], "pending_confirmation");
    assert_eq!(body["data"]["scope"], "workspace_marketing");

    // HEAD (link prefetch) → also side-effect free.
    let res = app.client.head(&url).send().await.unwrap();
    assert!(res.status().is_success());
    assert_public_headers(&res);

    // Repeat GET → still pending, nothing consumed, no suppression.
    let res = app.client.get(&url).send().await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        token_used_at(&app, world.workspace_id, &token).await,
        Some(None),
        "GET/HEAD must never consume the token"
    );
    assert_eq!(
        suppression_count(&app, world.workspace_id).await,
        0,
        "GET/HEAD must never suppress"
    );
}

/// T23b — unknown tokens get a generalized 404 that does not reveal
/// whether the address exists (B.4.4). Public headers also apply.
#[tokio::test]
async fn t23_unknown_token_generalized_404() {
    let Some((app, _)) = test_app().await else {
        return;
    };
    for method in ["GET", "POST"] {
        let res = app
            .client
            .request(
                method.parse().unwrap(),
                app.url("/public/v1/unsubscribe/never-issued-token"),
            )
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "{method}");
        assert_public_headers(&res);
        let p = res.json::<Value>().await.unwrap();
        assert_eq!(p["code"], "NOT_FOUND");
        // No address or token detail in the problem body.
        let s = p.to_string();
        assert!(!s.contains("example.com"), "{s}");
    }
}

/// T24 — a repeat POST on a valid token returns the same success and the
/// suppression stays single-row (B.4.4).
#[tokio::test]
async fn t24_repeat_post_same_success() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let token = issue_token(&app, &world, "t24").await;
    let url = app.url(&format!("/public/v1/unsubscribe/{token}"));

    let res = app.client.post(&url).json(&json!({})).send().await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_public_headers(&res);
    let first: Value = res.json().await.unwrap();
    assert_eq!(first["data"]["status"], "unsubscribed");
    assert_eq!(first["data"]["scope"], "workspace_marketing");
    assert_eq!(
        first["data"]["message"],
        "해당 발신 조직의 마케팅 이메일 수신 거부가 처리되었습니다."
    );

    // Empty body is accepted too (B.4.4: "empty object or empty").
    let res = app.client.post(&url).send().await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let second: Value = res.json().await.unwrap();
    assert_eq!(
        first["data"], second["data"],
        "repeat POST must return the same success"
    );

    // Non-empty/non-object bodies are rejected.
    let res = app
        .client
        .post(&url)
        .json(&json!({"email": "x@y.z"}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // One suppression only — the repeat reaffirmed it; the token is used.
    assert_eq!(suppression_count(&app, world.workspace_id).await, 1);
    assert!(
        matches!(
            token_used_at(&app, world.workspace_id, &token).await,
            Some(Some(_))
        ),
        "POST must consume the token"
    );

    // The token keeps resolving: GET now reports the completed state.
    let res = app.client.get(&url).send().await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["data"]["status"], "unsubscribed");
}

/// A used, nonrevoked token remains a capability to unsubscribe again after
/// the prior block is separately approved and released. Active repeats stay
/// quiet (no extra suppression events or pause jobs).
#[tokio::test]
async fn used_unsubscribe_token_resuppresses_after_approved_release() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let token = issue_token(&app, &world, "resubscribe-after-release").await;
    let url = app.url(&format!("/public/v1/unsubscribe/{token}"));
    let first = app.client.post(&url).json(&json!({})).send().await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(suppression_count(&app, world.workspace_id).await, 1);

    // Mirror an already-decided, non-self human approval and execute its
    // actual release job body in the same workspace transaction.
    let location = infrastructure::public_tokens::unsubscribe_token(&app.state.db, &token)
        .await
        .unwrap();
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let token_row = compliance_core::resolve_unsubscribe_token(&mut tx, &token)
        .await
        .unwrap()
        .unwrap();
    let suppression_id: Uuid = sqlx::query_scalar(
        "SELECT id FROM suppressions WHERE workspace_id=$1 AND email_address_id=$2
           AND purpose_scope='marketing' AND reason='unsubscribe' AND released_at IS NULL",
    )
    .bind(world.workspace_id)
    .bind(token_row.email_address_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let snapshot = infrastructure::crypto::sha256_bytes(b"unsubscribe re-consent review");
    let approval_request_id = approval_repo::insert_request(
        &mut tx,
        world.admin.principal_id,
        ApprovalTarget::SuppressionRelease(suppression_id),
        &snapshot,
        Utc::now() + Duration::days(7),
    )
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO approval_decisions
            (workspace_id,approval_request_id,decided_by,decision,snapshot_hash,reason,version_at_decision)
         VALUES ($1,$2,$3,'approved',$4,'recipient re-consented',1)",
    )
    .bind(world.workspace_id)
    .bind(approval_request_id)
    .bind(world.manager.principal_id)
    .bind(&snapshot[..])
    .execute(tx.conn())
    .await
    .unwrap();
    sqlx::query("UPDATE approval_requests SET status='approved' WHERE workspace_id=$1 AND id=$2")
        .bind(world.workspace_id)
        .bind(approval_request_id)
        .execute(tx.conn())
        .await
        .unwrap();
    let job_id = jobs::enqueue(
        &mut tx,
        "compliance.release_suppression",
        1,
        json!({"approval_request_id":approval_request_id,"suppression_id":suppression_id}),
        Utc::now(),
        Some(&format!("release_suppression:{approval_request_id}")),
    )
    .await
    .unwrap();
    application::compliance::release::handle(
        &mut tx,
        job_id,
        &json!({"approval_request_id":approval_request_id,"suppression_id":suppression_id}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let confirmation = app.client.get(&url).send().await.unwrap();
    assert_eq!(confirmation.status(), StatusCode::OK);
    let confirmation: Value = confirmation.json().await.unwrap();
    assert_eq!(confirmation["data"]["status"], "pending_confirmation");

    let resuppress = app.client.post(&url).json(&json!({})).send().await.unwrap();
    assert_eq!(resuppress.status(), StatusCode::OK);
    assert_eq!(
        resuppress.json::<Value>().await.unwrap()["data"]["status"],
        "unsubscribed"
    );
    assert!(matches!(
        token_used_at(&app, world.workspace_id, &token).await,
        Some(Some(_))
    ));

    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM suppressions WHERE workspace_id=$1 AND email_address_id=$2
           AND released_at IS NULL AND (purpose_scope='all' OR purpose_scope='marketing')",
    )
    .bind(world.workspace_id)
    .bind(token_row.email_address_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(active, 1);
    let events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM outbox_events WHERE workspace_id=$1 AND event_type='suppression.created'
           AND payload->>'email_address_id'=$2",
    )
    .bind(world.workspace_id)
    .bind(token_row.email_address_id.to_string())
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let pause_jobs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE workspace_id=$1 AND kind='compliance.pause_enrollments'
           AND payload->>'email_address_id'=$2",
    )
    .bind(world.workspace_id)
    .bind(token_row.email_address_id.to_string())
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.rollback().await.unwrap();

    let repeated = app.client.post(&url).json(&json!({})).send().await.unwrap();
    assert_eq!(repeated.status(), StatusCode::OK);
    assert_eq!(suppression_count(&app, world.workspace_id).await, 2);
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let events_after: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM outbox_events WHERE workspace_id=$1 AND event_type='suppression.created'
           AND payload->>'email_address_id'=$2",
    )
    .bind(world.workspace_id)
    .bind(token_row.email_address_id.to_string())
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let jobs_after: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE workspace_id=$1 AND kind='compliance.pause_enrollments'
           AND payload->>'email_address_id'=$2",
    )
    .bind(world.workspace_id)
    .bind(token_row.email_address_id.to_string())
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(events_after, events);
    assert_eq!(jobs_after, pause_jobs);
    assert_eq!(location.workspace_id, world.workspace_id);
}

/// T25 — RFC 8058 one-click exists but returns the documented disabled
/// response while the capability gate is off (never a silent success).
#[tokio::test]
async fn t25_one_click_returns_disabled_until_gated() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let token = issue_token(&app, &world, "t25").await;

    // RFC 8058 form body, no cookies — the gate off means no suppression.
    let res = app
        .client
        .post(app.url(&format!("/public/v1/unsubscribe/{token}/one-click")))
        .header("content-type", "application/x-www-form-urlencoded")
        .body("List-Unsubscribe=One-Click")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_public_headers(&res);
    let p = res.json::<Value>().await.unwrap();
    assert_eq!(p["code"], "PROVIDER_POLICY_UNVERIFIED");

    // Critically: the suppressed row was NOT created — one-click must not
    // silently "succeed".
    assert_eq!(suppression_count(&app, world.workspace_id).await, 0);
    assert!(
        matches!(
            token_used_at(&app, world.workspace_id, &token).await,
            Some(None)
        ),
        "one-click must not consume the token while disabled"
    );

    // Unknown token still gets the generalized 404 before the gate check.
    let res = app
        .client
        .post(app.url("/public/v1/unsubscribe/not-a-token/one-click"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body("List-Unsubscribe=One-Click")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

/// Internal `/communication-bases` — pending on create, human review under
/// If-Match, final states enforced, revoke withdraws (B.2, A.6.1).
#[tokio::test]
async fn bases_create_review_revoke_flow() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let csrf = admin_session(&app, &world).await;

    // An address row to attach the basis to.
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let addr = insert_email_address(&mut tx, "basis-flow").await;
    tx.commit().await.unwrap();

    // Create — always pending; clients may not pre-seed review state.
    let res = app
        .post(
            "/api/v1/communication-bases",
            json!({
                "email_address_id": addr,
                "purpose": "marketing",
                "jurisdiction": "KR",
                "basis_type": "explicit_consent",
                "evidence_ref": "consent-form-2026-10-09",
                "collected_at": "2026-10-01T00:00:00Z"
            }),
            Some(&csrf),
        )
        .await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let etag = res
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .expect("versioned mutation must return ETag");
    assert_eq!(etag, "\"1\"");
    let created = res.json::<Value>().await.unwrap();
    let basis_id = created["data"]["id"].as_str().unwrap().to_string();
    assert_eq!(created["data"]["status"], "pending");
    assert!(created["data"]["reviewed_by"].is_null());

    // System-owned fields are rejected outright (B.1.1).
    let res = app
        .post(
            "/api/v1/communication-bases",
            json!({
                "email_address_id": addr,
                "purpose": "marketing",
                "jurisdiction": "KR",
                "basis_type": "explicit_consent",
                "evidence_ref": "e",
                "collected_at": "2026-10-01T00:00:00Z",
                "status": "valid"
            }),
            Some(&csrf),
        )
        .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // Review without If-Match → 428.
    let res = app
        .post(
            &format!("/api/v1/communication-bases/{basis_id}/reviews"),
            json!({"decision": "valid"}),
            Some(&csrf),
        )
        .await;
    let p = TestApp::problem(res, StatusCode::from_u16(428).unwrap()).await;
    assert_eq!(p["code"], "VERSION_PRECONDITION_REQUIRED");

    // Stale If-Match → 412.
    let res = app
        .client
        .post(app.url(&format!("/api/v1/communication-bases/{basis_id}/reviews")))
        .header("if-match", "\"99\"")
        .header("x-csrf-token", &csrf)
        .json(&json!({"decision": "valid"}))
        .send()
        .await
        .unwrap();
    let p = TestApp::problem(res, StatusCode::PRECONDITION_FAILED).await;
    assert_eq!(p["code"], "VERSION_MISMATCH");

    // Correct version → valid.
    let res = app
        .client
        .post(app.url(&format!("/api/v1/communication-bases/{basis_id}/reviews")))
        .header("if-match", "\"1\"")
        .header("x-csrf-token", &csrf)
        .json(&json!({"decision": "valid", "valid_until": "2027-10-01T00:00:00Z"}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let reviewed = res.json::<Value>().await.unwrap();
    assert_eq!(reviewed["data"]["status"], "valid");
    assert_eq!(
        reviewed["data"]["reviewed_by"],
        world.admin.principal_id.to_string()
    );
    assert_eq!(reviewed["data"]["version"], 2);

    // A decided basis is final — a second decision conflicts.
    let res = app
        .client
        .post(app.url(&format!("/api/v1/communication-bases/{basis_id}/reviews")))
        .header("if-match", "\"2\"")
        .header("x-csrf-token", &csrf)
        .json(&json!({"decision": "rejected"}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);

    // Revoke needs the current version.
    let res = app
        .client
        .post(app.url(&format!("/api/v1/communication-bases/{basis_id}/revoke")))
        .header("if-match", "\"2\"")
        .header("x-csrf-token", &csrf)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let revoked = res.json::<Value>().await.unwrap();
    assert_eq!(revoked["data"]["status"], "revoked");
    assert!(revoked["data"]["revoked_at"].is_string());
}

/// `/suppressions` — create via the shared core, idempotent reaffirm,
/// release-requests create a pending approval (never a direct release).
#[tokio::test]
async fn suppressions_create_and_release_request_flow() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let csrf = admin_session(&app, &world).await;

    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let addr = insert_email_address(&mut tx, "supp-flow").await;
    tx.commit().await.unwrap();

    let body = json!({
        "email_address_id": addr,
        "purpose_scope": "marketing",
        "reason": "manual"
    });
    let res = app
        .post("/api/v1/suppressions", body.clone(), Some(&csrf))
        .await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let created = res.json::<Value>().await.unwrap();
    let sid = created["data"]["id"].as_str().unwrap().to_string();
    assert_eq!(created["data"]["status"], "active");
    assert_eq!(
        created["data"]["created_by"],
        world.admin.principal_id.to_string()
    );

    // Identical repeat → 200 reaffirm, same row, no duplicate.
    let res = app.post("/api/v1/suppressions", body, Some(&csrf)).await;
    assert_eq!(res.status(), StatusCode::OK);
    let again = res.json::<Value>().await.unwrap();
    assert_eq!(again["data"]["id"], sid);
    assert_eq!(suppression_count(&app, world.workspace_id).await, 1);

    // Validation: bad scope / missing address.
    let res = app
        .post(
            "/api/v1/suppressions",
            json!({"email_address_id": addr, "purpose_scope": "everything", "reason": "manual"}),
            Some(&csrf),
        )
        .await;
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let res = app
        .post(
            "/api/v1/suppressions",
            json!({"email_address_id": Uuid::new_v4(), "purpose_scope": "marketing", "reason": "manual"}),
            Some(&csrf),
        )
        .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    // List — the new row is visible, filtered views work.
    let res = app
        .get("/api/v1/suppressions?purpose_scope=marketing&status=active")
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let list = res.json::<Value>().await.unwrap();
    let ids: Vec<&str> = list["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["id"].as_str())
        .collect();
    assert!(ids.contains(&sid.as_str()), "list missing {sid}");

    // Release-requests: evidence + reason → pending approval (B.2) — never
    // a direct release.
    let res = app
        .post(
            &format!("/api/v1/suppressions/{sid}/release-requests"),
            json!({"evidence_ref": "re-consent-2026", "reason": "recipient re-consented via form"}),
            Some(&csrf),
        )
        .await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let req = res.json::<Value>().await.unwrap();
    assert_eq!(req["data"]["status"], "pending");
    assert_eq!(req["data"]["suppression_id"], sid);

    // Second pending request for the same suppression → 409 REQUEST_IN_PROGRESS.
    let res = app
        .post(
            &format!("/api/v1/suppressions/{sid}/release-requests"),
            json!({"evidence_ref": "e2", "reason": "duplicate"}),
            Some(&csrf),
        )
        .await;
    let p = TestApp::problem(res, StatusCode::CONFLICT).await;
    assert_eq!(p["code"], "REQUEST_IN_PROGRESS");

    // The suppression itself is untouched — release goes through the
    // approval + job path, not this endpoint.
    let res = app.get(&format!("/api/v1/suppressions/{sid}")).await;
    let s = res.json::<Value>().await.unwrap();
    assert_eq!(s["data"]["status"], "active");
    assert!(s["data"]["released_at"].is_null());
}

/// Authz: unauthenticated → 401; member may read but not review/release.
#[tokio::test]
async fn authz_boundaries() {
    let Some((app, world)) = test_app().await else {
        return;
    };

    // No session → 401 on internal routes (but public routes below are open).
    let res = app.get("/api/v1/suppressions").await;
    TestApp::problem(res, StatusCode::UNAUTHORIZED).await;
    let res = app.get("/api/v1/communication-bases").await;
    TestApp::problem(res, StatusCode::UNAUTHORIZED).await;

    // Member login — read allowed, release-request denied (manager+ only).
    let res = app
        .login(world.member_a.email.as_deref().unwrap(), TEST_PASSWORD)
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let csrf = TestApp::csrf_token_from(&res).unwrap();

    let res = app.get("/api/v1/suppressions").await;
    assert_eq!(res.status(), StatusCode::OK);

    let res = app
        .post(
            &format!("/api/v1/suppressions/{}/release-requests", Uuid::new_v4()),
            json!({"evidence_ref": "e", "reason": "r"}),
            Some(&csrf),
        )
        .await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    let res = app
        .client
        .post(app.url(&format!(
            "/api/v1/communication-bases/{}/reviews",
            Uuid::new_v4()
        )))
        .header("if-match", "\"1\"")
        .header("x-csrf-token", &csrf)
        .json(&json!({"decision": "valid"}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

/// §5.3 limited add — a Member may only create suppressions for addresses
/// inside their CRM/mailbox-visible scope. An unlinked address is
/// out-of-scope → 403 (also hides existence); a contact-linked one is
/// in-scope → 201.
#[tokio::test]
async fn member_suppression_requires_visible_scope() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let res = app
        .login(world.member_a.email.as_deref().unwrap(), TEST_PASSWORD)
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let csrf = TestApp::csrf_token_from(&res).unwrap();

    // Out of scope: the address exists but is linked to nothing the member
    // can see → 403 (existence not disclosed).
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let stray = insert_email_address(&mut tx, "member-stray").await;
    tx.commit().await.unwrap();

    let res = app
        .post(
            "/api/v1/suppressions",
            json!({"email_address_id": stray, "purpose_scope": "marketing", "reason": "manual"}),
            Some(&csrf),
        )
        .await;
    let p = TestApp::problem(res, StatusCode::FORBIDDEN).await;
    assert_eq!(p["code"], "FORBIDDEN");

    // In scope: the address is linked to a contact (shared CRM read).
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let addr = insert_email_address(&mut tx, "member-linked").await;
    let contact = fixtures::insert_contact(&mut tx, "Linked Person")
        .await
        .unwrap();
    fixtures::link_contact_email(&mut tx, contact, addr)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let res = app
        .post(
            "/api/v1/suppressions",
            json!({"email_address_id": addr, "purpose_scope": "marketing", "reason": "manual"}),
            Some(&csrf),
        )
        .await;
    assert_eq!(res.status(), StatusCode::CREATED);
}

/// Mint a service credential for the test agent principal.
async fn agent_token(app: &TestApp, world: &TestWorld, scopes: &[&str]) -> String {
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let (_id, token) = infrastructure::repo::auth::create_service_credential(
        &mut tx,
        world.agent.principal_id,
        "test-agent",
        &scopes.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        chrono::Duration::hours(1),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    token
}

/// §5.3 limited add for agents: in-scope (contact-linked address +
/// `crm.read` credential) → 201; out-of-scope → 403 even though the agent
/// holds a valid credential.
#[tokio::test]
async fn agent_suppression_scope_limited() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let token = agent_token(&app, &world, &["crm.read"]).await;

    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let linked = insert_email_address(&mut tx, "agent-linked").await;
    let contact = fixtures::insert_contact(&mut tx, "Agent Visible")
        .await
        .unwrap();
    fixtures::link_contact_email(&mut tx, contact, linked)
        .await
        .unwrap();
    let stray = insert_email_address(&mut tx, "agent-stray").await;
    tx.commit().await.unwrap();

    // Out of scope → 403.
    let res = app
        .client
        .post(app.url("/api/v1/suppressions"))
        .bearer_auth(&token)
        .json(&json!({"email_address_id": stray, "purpose_scope": "marketing", "reason": "manual"}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    // In scope → 201.
    let res = app
        .client
        .post(app.url("/api/v1/suppressions"))
        .bearer_auth(&token)
        .json(
            &json!({"email_address_id": linked, "purpose_scope": "marketing", "reason": "manual"}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);

    // An agent credential with no scopes is out-of-scope even for a
    // contact-linked address (no crm.read / mail.read).
    let scopeless = agent_token(&app, &world, &[]).await;
    let res = app
        .client
        .post(app.url("/api/v1/suppressions"))
        .bearer_auth(&scopeless)
        .json(&json!({"email_address_id": linked, "purpose_scope": "all", "reason": "complaint"}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

/// §5.3 "제한적 추가" quota: the 21st suppression create by the same agent
/// credential within an hour is 429 RATE_LIMITED.
#[tokio::test]
async fn agent_suppression_create_quota_429() {
    let Some(rl) = redis_limiter().await else {
        return;
    };
    let Some((app, world)) = test_app_with(Some(rl)).await else {
        return;
    };
    let token = agent_token(&app, &world, &["crm.read"]).await;

    // 21 distinct in-scope addresses — each create is real (201).
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let contact = fixtures::insert_contact(&mut tx, "Quota Target")
        .await
        .unwrap();
    let mut addrs = Vec::new();
    for i in 0..21 {
        let a = insert_email_address(&mut tx, &format!("quota-{i}")).await;
        fixtures::link_contact_email(&mut tx, contact, a)
            .await
            .unwrap();
        addrs.push(a);
    }
    tx.commit().await.unwrap();

    for (i, addr) in addrs.iter().enumerate() {
        let res = app
            .client
            .post(app.url("/api/v1/suppressions"))
            .bearer_auth(&token)
            .json(&json!({"email_address_id": addr, "purpose_scope": "marketing", "reason": "manual"}))
            .send()
            .await
            .unwrap();
        if i < 20 {
            assert_eq!(res.status(), StatusCode::CREATED, "create {i}");
        } else {
            let p = TestApp::problem(res, StatusCode::TOO_MANY_REQUESTS).await;
            assert_eq!(p["code"], "RATE_LIMITED");
        }
    }
}
