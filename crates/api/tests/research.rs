//! Task 5 (research) API tests — HTTP-level behavior only (the api crate
//! has no sqlx dep; DB-level assertions live in worker/tests/research.rs).
//!
//! - Batch rules: ≤100, all-or-nothing 422 with per-item violations.
//! - Evidence `source_url` SSRF policy (§15.1) rejects non-public targets.
//! - Decisions require human + `research.write` + `If-Match` (428/412/409).
//! - `POST /lead-proposals` idempotent replay + `research.write` scoping.
//! - Evidence reviews: pending → verified → terminal 409.

use api::AppState;
use api::testing::TestApp;
use infrastructure::config::Config;
use infrastructure::db::{Db, ROLE_API};
use infrastructure::testing::{TEST_PASSWORD, seed_workspace};
use reqwest::StatusCode;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

async fn test_app() -> Option<(TestApp, infrastructure::testing::TestWorld)> {
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

async fn login_csrf(app: &TestApp, email: &str) -> String {
    let res = app.login(email, TEST_PASSWORD).await;
    assert_eq!(res.status(), StatusCode::OK, "login failed");
    TestApp::csrf_token_from(&res).unwrap()
}

fn proposal(company: &str, email: Option<&str>) -> serde_json::Value {
    let contacts = match email {
        Some(e) => json!([{"name": "Kim Research", "email": e, "job_title": "VP"}]),
        None => json!([]),
    };
    json!({
        "proposals": [{
            "external_key": format!("ext-{company}"),
            "company_name": company,
            "domain": format!("{company}.example"),
            "proposed_data": {"industry": "mfg", "contacts": contacts},
            "evidence": [{
                "field_path": "industry",
                "source_url": "https://public.example/source",
                "observed_at": "2026-01-01T00:00:00Z",
                "value": "mfg",
                "evidence_type": "observed",
                "confidence": 0.9
            }]
        }]
    })
}

async fn create_ok(app: &TestApp, csrf: &str, body: serde_json::Value) -> serde_json::Value {
    let res = app.post("/api/v1/lead-proposals", body, Some(csrf)).await;
    assert_eq!(res.status(), StatusCode::CREATED, "create failed");
    TestApp::data(res).await
}

#[tokio::test]
async fn proposals_batch_rules_and_evidence_url_policy() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let csrf = login_csrf(&app, world.admin.email.as_deref().unwrap()).await;

    // >100 items → 422, all-or-nothing.
    let big = json!({
        "proposals": (0..=100).map(|i| json!({
            "company_name": format!("C{i}"),
            "domain": "x.example",
            "evidence": [{
                "field_path": "f", "source_url": "https://ok.example",
                "observed_at": "2026-01-01T00:00:00Z", "value": 1,
                "evidence_type": "observed"
            }]
        })).collect::<Vec<_>>()
    });
    let res = app.post("/api/v1/lead-proposals", big, Some(&csrf)).await;
    let p = TestApp::problem(res, StatusCode::UNPROCESSABLE_ENTITY).await;
    assert_eq!(p["code"], "VALIDATION_FAILED");

    // One SSRF URL + one missing evidence → whole batch 422 with indexed
    // violation fields.
    let mixed = json!({
        "proposals": [
            {
                "company_name": "Good", "domain": "good.example",
                "evidence": [{
                    "field_path": "f", "source_url": "https://ok.example",
                    "observed_at": "2026-01-01T00:00:00Z", "value": 1,
                    "evidence_type": "observed"
                }]
            },
            {
                "company_name": "Bad", "domain": "bad.example",
                "evidence": [{
                    "field_path": "f", "source_url": "http://127.0.0.1/x",
                    "observed_at": "2026-01-01T00:00:00Z", "value": 1,
                    "evidence_type": "observed"
                }]
            },
            {"company_name": "NoEvidence", "domain": "ne.example", "evidence": []}
        ]
    });
    let res = app.post("/api/v1/lead-proposals", mixed, Some(&csrf)).await;
    let p = TestApp::problem(res, StatusCode::UNPROCESSABLE_ENTITY).await;
    let violations = p["violations"].as_array().cloned().unwrap_or_default();
    assert!(
        violations.iter().any(
            |v| v["field"].as_str().unwrap_or("").contains("proposals[1]")
                && v["code"].as_str().unwrap_or("").contains("invalid_url")
        ),
        "expected SSRF violation: {violations:?}"
    );
    assert!(
        violations
            .iter()
            .any(|v| v["field"].as_str().unwrap_or("") == "proposals[2].evidence"),
        "expected missing-evidence violation: {violations:?}"
    );

    // Nothing of the failed batch was persisted.
    let res = app.get("/api/v1/lead-proposals").await;
    let data = TestApp::data(res).await;
    assert_eq!(data.as_array().unwrap().len(), 0);

    // Unknown fields are rejected (deny_unknown_fields).
    let res = app
        .post(
            "/api/v1/lead-proposals",
            json!({"proposals": [], "workspace_id": Uuid::new_v4()}),
            Some(&csrf),
        )
        .await;
    TestApp::problem(res, StatusCode::BAD_REQUEST).await;
}

#[tokio::test]
async fn decision_version_authz_and_evidence_reviews() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let csrf = login_csrf(&app, world.admin.email.as_deref().unwrap()).await;
    let data = create_ok(&app, &csrf, proposal("acme", Some("kim@acme.example"))).await;
    let proposal_id: Uuid = data["proposals"][0]["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    // Duplicate-company hint is reported (never auto-merged).
    assert!(data["proposals"][0]["duplicate_company_ids"].is_array());

    // Detail: version 1 + attached evidence, ETag header set.
    let res = app
        .get(&format!("/api/v1/lead-proposals/{proposal_id}"))
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert!(res.headers().get("etag").is_some());
    let detail = TestApp::data(res).await;
    assert_eq!(detail["version"], 1);
    assert_eq!(detail["evidence"].as_array().unwrap().len(), 1);
    let ev_id = detail["evidence"][0]["id"].as_str().unwrap().to_string();

    // Missing If-Match → 428.
    let res = app
        .post(
            &format!("/api/v1/lead-proposals/{proposal_id}/decisions"),
            json!({"decision": "accept"}),
            Some(&csrf),
        )
        .await;
    let p = TestApp::problem(res, StatusCode::PRECONDITION_REQUIRED).await;
    assert_eq!(p["code"], "VERSION_PRECONDITION_REQUIRED");

    // Stale If-Match → 412.
    let res = app
        .client
        .post(app.url(&format!("/api/v1/lead-proposals/{proposal_id}/decisions")))
        .header("x-csrf-token", &csrf)
        .header("if-match", "\"99\"")
        .json(&json!({"decision": "accept"}))
        .send()
        .await
        .unwrap();
    let p = TestApp::problem(res, StatusCode::PRECONDITION_FAILED).await;
    assert_eq!(p["code"], "VERSION_MISMATCH");

    // Accept → new version + created ids (rows verified DB-side in
    // worker/tests/research.rs).
    let res = app
        .client
        .post(app.url(&format!("/api/v1/lead-proposals/{proposal_id}/decisions")))
        .header("x-csrf-token", &csrf)
        .header("if-match", "\"1\"")
        .json(&json!({"decision": "accept"}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let data = TestApp::data(res).await;
    assert_eq!(data["status"], "accepted");
    assert!(data["accepted_company_id"].is_string());
    assert_eq!(data["contact_ids"].as_array().unwrap().len(), 1);
    assert_eq!(data["email_address_ids"].as_array().unwrap().len(), 1);
    assert_eq!(data["version"], 2);

    // Second decision on the terminal row → 409.
    let res = app
        .client
        .post(app.url(&format!("/api/v1/lead-proposals/{proposal_id}/decisions")))
        .header("x-csrf-token", &csrf)
        .header("if-match", "\"2\"")
        .json(&json!({"decision": "reject"}))
        .send()
        .await
        .unwrap();
    TestApp::problem(res, StatusCode::CONFLICT).await;

    // Evidence review: pending → verified (admin has research.write);
    // second review → 409 terminal.
    let res = app
        .post(
            &format!("/api/v1/research-evidence/{ev_id}/reviews"),
            json!({"decision": "verified"}),
            Some(&csrf),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(TestApp::data(res).await["review_status"], "verified");
    let res = app
        .post(
            &format!("/api/v1/research-evidence/{ev_id}/reviews"),
            json!({"decision": "rejected"}),
            Some(&csrf),
        )
        .await;
    TestApp::problem(res, StatusCode::CONFLICT).await;

    // Member lacks research.write → 403. NOTE: TestApp shares one cookie
    // jar — the member login replaces the session, so member checks run
    // last (scope is evaluated before the row lookup, so terminal rows
    // still produce a clean 403).
    let member_csrf = login_csrf(&app, world.member_a.email.as_deref().unwrap()).await;
    let res = app
        .client
        .post(app.url(&format!("/api/v1/lead-proposals/{proposal_id}/decisions")))
        .header("x-csrf-token", &member_csrf)
        .header("if-match", "\"2\"")
        .json(&json!({"decision": "reject"}))
        .send()
        .await
        .unwrap();
    TestApp::problem(res, StatusCode::FORBIDDEN).await;
    let res = app
        .post(
            &format!("/api/v1/research-evidence/{ev_id}/reviews"),
            json!({"decision": "rejected"}),
            Some(&member_csrf),
        )
        .await;
    TestApp::problem(res, StatusCode::FORBIDDEN).await;
}

#[tokio::test]
async fn idempotent_create_replays_and_rejects_reuse() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let csrf = login_csrf(&app, world.admin.email.as_deref().unwrap()).await;

    let body = proposal("idemp", None);
    let res = app
        .client
        .post(app.url("/api/v1/lead-proposals"))
        .header("x-csrf-token", &csrf)
        .header("idempotency-key", "k-1")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let first = TestApp::data(res).await;

    let res = app
        .client
        .post(app.url("/api/v1/lead-proposals"))
        .header("x-csrf-token", &csrf)
        .header("idempotency-key", "k-1")
        .json(&body)
        .send()
        .await
        .unwrap();
    let second = TestApp::data(res).await;
    assert_eq!(first, second, "same key+body must replay");

    // Same key, different body → 409 IDEMPOTENCY_KEY_REUSED.
    let res = app
        .client
        .post(app.url("/api/v1/lead-proposals"))
        .header("x-csrf-token", &csrf)
        .header("idempotency-key", "k-1")
        .json(&proposal("other", None))
        .send()
        .await
        .unwrap();
    let p = TestApp::problem(res, StatusCode::CONFLICT).await;
    assert_eq!(p["code"], "IDEMPOTENCY_KEY_REUSED");

    // List shows exactly the agent-less proposals so far.
    let res = app.get("/api/v1/lead-proposals?status=pending").await;
    let data = TestApp::data(res).await;
    assert_eq!(data.as_array().unwrap().len(), 1);
}
