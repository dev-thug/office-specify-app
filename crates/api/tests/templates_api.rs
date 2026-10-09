//! templates module API tests (B.3.5, §8.1): CRUD + immutable versions +
//! the allowlist render endpoint.

use api::AppState;
use api::testing::TestApp;
use infrastructure::config::Config;
use infrastructure::db::{Db, ROLE_API};
use infrastructure::testing::{TEST_PASSWORD, TestWorld, seed_workspace};
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::sync::Arc;

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

/// Login and return the CSRF token (session cookie stays in the jar).
async fn csrf(app: &TestApp, email: &str) -> String {
    let res = app.login(email, TEST_PASSWORD).await;
    assert_eq!(res.status(), StatusCode::OK, "login failed");
    TestApp::csrf_token_from(&res).expect("csrf cookie missing")
}

async fn post(app: &TestApp, path: &str, body: Value, csrf: &str) -> reqwest::Response {
    app.client
        .post(app.url(path))
        .header("x-csrf-token", csrf)
        .json(&body)
        .send()
        .await
        .unwrap()
}

async fn patch(
    app: &TestApp,
    path: &str,
    body: Value,
    csrf: &str,
    if_match: Option<i64>,
) -> reqwest::Response {
    let mut req = app.client.patch(app.url(path)).header("x-csrf-token", csrf);
    if let Some(v) = if_match {
        req = req.header("if-match", format!("\"{v}\""));
    }
    req.json(&body).send().await.unwrap()
}

fn version_body() -> Value {
    json!({
        "subject": "{{company.name}} 안내",
        "body_text": "안녕하세요 {{contact.name}}님 — {{meeting.booking_url}}",
        "body_html": "<p>{{company.name}}</p>",
        "variable_schema": {
            "type": "object",
            "properties": {
                "company.name": {"type": "string", "maxLength": 300},
                "contact.name": {"type": "string", "maxLength": 300},
                "meeting.booking_url": {"type": "string", "format": "uri"}
            },
            "required": ["company.name", "contact.name"],
            "additionalProperties": false
        }
    })
}

async fn make_template(app: &TestApp, csrf: &str, name: &str) -> Value {
    let res = post(app, "/api/v1/email-templates", json!({"name": name}), csrf).await;
    TestApp::data(res).await
}

#[tokio::test]
async fn template_crud_and_versioning() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let csrf = csrf(&app, world.admin.email.as_deref().unwrap()).await;

    // Create → 201 + ETag.
    let res = post(
        &app,
        "/api/v1/email-templates",
        json!({"name": "intro"}),
        &csrf,
    )
    .await;
    assert_eq!(res.status(), StatusCode::CREATED);
    assert_eq!(res.headers().get("etag").unwrap(), "\"1\"");
    let tpl = TestApp::data(res).await;
    let tpl_id = tpl["template_id"].as_str().unwrap();

    // Get.
    let res = app.get(&format!("/api/v1/email-templates/{tpl_id}")).await;
    let got = TestApp::data(res).await;
    assert_eq!(got["name"], "intro");
    assert_eq!(got["version"], 1);

    // List contains it; other workspaces' rows never leak (RLS).
    let res = app.get("/api/v1/email-templates").await;
    let list = TestApp::data(res).await;
    assert_eq!(list.as_array().unwrap().len(), 1);

    // PATCH without If-Match → 428.
    let res = patch(
        &app,
        &format!("/api/v1/email-templates/{tpl_id}"),
        json!({"name": "renamed"}),
        &csrf,
        None,
    )
    .await;
    let p = TestApp::problem(res, StatusCode::from_u16(428).unwrap()).await;
    assert_eq!(p["code"], "VERSION_PRECONDITION_REQUIRED");

    // PATCH with stale version → 412.
    let res = patch(
        &app,
        &format!("/api/v1/email-templates/{tpl_id}"),
        json!({"name": "renamed"}),
        &csrf,
        Some(99),
    )
    .await;
    let p = TestApp::problem(res, StatusCode::PRECONDITION_FAILED).await;
    assert_eq!(p["code"], "VERSION_MISMATCH");

    // PATCH with correct version → 200 + bumped ETag.
    let res = patch(
        &app,
        &format!("/api/v1/email-templates/{tpl_id}"),
        json!({"name": "renamed"}),
        &csrf,
        Some(1),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers().get("etag").unwrap(), "\"2\"");
    let patched = TestApp::data(res).await;
    assert_eq!(patched["name"], "renamed");

    // deny_unknown_fields → 422 VALIDATION_FAILED.
    let res = post(
        &app,
        "/api/v1/email-templates",
        json!({"name": "x", "workspace_id": uuid::Uuid::new_v4()}),
        &csrf,
    )
    .await;
    let p = TestApp::problem(res, StatusCode::BAD_REQUEST).await;
    assert_eq!(p["code"], "VALIDATION_FAILED");
}

#[tokio::test]
async fn template_versions_immutable_and_render() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let csrf = csrf(&app, world.admin.email.as_deref().unwrap()).await;

    let tpl = make_template(&app, &csrf, "versioned").await;
    let tpl_id = tpl["template_id"].as_str().unwrap().to_string();

    // Version 1 → 201 {template_version_id, version_no, content_hash}.
    let res = post(
        &app,
        &format!("/api/v1/email-templates/{tpl_id}/versions"),
        version_body(),
        &csrf,
    )
    .await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let v1 = TestApp::data(res).await;
    assert_eq!(v1["version_no"], 1);
    let v1_id = v1["template_version_id"].as_str().unwrap().to_string();
    let v1_hash = v1["content_hash"].as_str().unwrap().to_string();
    assert_eq!(v1_hash.len(), 64);

    // Version 2 → version_no 2, different hash.
    let mut body2 = version_body();
    body2["subject"] = json!("v2 {{company.name}}");
    let res = post(
        &app,
        &format!("/api/v1/email-templates/{tpl_id}/versions"),
        body2,
        &csrf,
    )
    .await;
    let v2 = TestApp::data(res).await;
    assert_eq!(v2["version_no"], 2);
    assert_ne!(v2["content_hash"], v1_hash);

    // Version list — newest first, v1 row intact.
    let res = app
        .get(&format!("/api/v1/email-templates/{tpl_id}/versions"))
        .await;
    let versions = TestApp::data(res).await;
    let arr = versions.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["version_no"], 2);
    assert_eq!(arr[1]["version_no"], 1);

    // Render v1 with declared variables → preview with system preview URL.
    let res = post(
        &app,
        &format!("/api/v1/template-versions/{v1_id}/render"),
        json!({"variables": {"company.name": "Acme & Co", "contact.name": "Kim"}}),
        &csrf,
    )
    .await;
    let out = TestApp::data(res).await;
    assert_eq!(out["subject"], "Acme & Co 안내");
    assert!(out["body_text"].as_str().unwrap().contains("Kim님"));
    // The booking URL is server-injected — the preview placeholder, never
    // a real link and never a caller-supplied value.
    assert!(
        out["body_text"]
            .as_str()
            .unwrap()
            .contains("preview.invalid"),
        "system variable must render as the preview URL: {out}"
    );
    // HTML part is escaped.
    assert_eq!(out["body_html"].as_str().unwrap(), "<p>Acme &amp; Co</p>");

    // Caller may not inject a system variable → 422.
    let res = post(
        &app,
        &format!("/api/v1/template-versions/{v1_id}/render"),
        json!({"variables": {
            "company.name": "x", "contact.name": "y",
            "meeting.booking_url": "https://evil.example"
        }}),
        &csrf,
    )
    .await;
    let p = TestApp::problem(res, StatusCode::BAD_REQUEST).await;
    assert_eq!(p["code"], "VALIDATION_FAILED");

    // Missing required variable → 422 TEMPLATE_VARIABLE_MISSING.
    let res = post(
        &app,
        &format!("/api/v1/template-versions/{v1_id}/render"),
        json!({"variables": {"company.name": "x"}}),
        &csrf,
    )
    .await;
    let p = TestApp::problem(res, StatusCode::UNPROCESSABLE_ENTITY).await;
    assert_eq!(p["code"], "TEMPLATE_VARIABLE_MISSING");
}

#[tokio::test]
async fn template_version_content_guards() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let csrf = csrf(&app, world.admin.email.as_deref().unwrap()).await;
    let tpl = make_template(&app, &csrf, "guards").await;
    let tpl_id = tpl["template_id"].as_str().unwrap().to_string();
    let versions = format!("/api/v1/email-templates/{tpl_id}/versions");

    // Undeclared placeholder in the body → 422 (allowlist renderer).
    let mut bad = version_body();
    bad["body_text"] = json!("{{evil.expression | danger}}");
    let res = post(&app, &versions, bad, &csrf).await;
    let p = TestApp::problem(res, StatusCode::BAD_REQUEST).await;
    assert_eq!(p["code"], "VALIDATION_FAILED");

    // Dangerous HTML → 422.
    let mut bad = version_body();
    bad["body_html"] = json!("<script>alert(1)</script>");
    let res = post(&app, &versions, bad, &csrf).await;
    let p = TestApp::problem(res, StatusCode::BAD_REQUEST).await;
    assert_eq!(p["code"], "VALIDATION_FAILED");

    // CR/LF in the subject → 422 (header injection).
    let mut bad = version_body();
    bad["subject"] = json!("Hi\nBcc: x@y.z");
    let res = post(&app, &versions, bad, &csrf).await;
    let p = TestApp::problem(res, StatusCode::BAD_REQUEST).await;
    assert_eq!(p["code"], "VALIDATION_FAILED");

    // Archived template cannot take new versions → 409.
    let res = patch(
        &app,
        &format!("/api/v1/email-templates/{tpl_id}"),
        json!({"archived": true}),
        &csrf,
        Some(1),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let res = post(&app, &versions, version_body(), &csrf).await;
    TestApp::problem(res, StatusCode::CONFLICT).await;
}

#[tokio::test]
async fn template_scope_enforcement() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    // member_a has templates.read but NOT templates.write (Member role).
    let member_csrf = csrf(&app, world.member_a.email.as_deref().unwrap()).await;
    let res = post(
        &app,
        "/api/v1/email-templates",
        json!({"name": "nope"}),
        &member_csrf,
    )
    .await;
    let p = TestApp::problem(res, StatusCode::FORBIDDEN).await;
    assert_eq!(p["code"], "SCOPE_REQUIRED");

    // Read is allowed for the same member.
    let res = app.get("/api/v1/email-templates").await;
    TestApp::data(res).await;
}

#[tokio::test]
async fn template_mutations_emit_outbox_events_in_the_business_transaction() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let csrf = csrf(&app, world.admin.email.as_deref().unwrap()).await;
    let tpl = make_template(&app, &csrf, "outbox").await;
    let tpl_id = tpl["template_id"].as_str().unwrap();

    let res = post(
        &app,
        &format!("/api/v1/email-templates/{tpl_id}/versions"),
        version_body(),
        &csrf,
    )
    .await;
    TestApp::data(res).await;
    let res = patch(
        &app,
        &format!("/api/v1/email-templates/{tpl_id}"),
        json!({"name": "outbox-updated"}),
        &csrf,
        Some(1),
    )
    .await;
    TestApp::data(res).await;

    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let events: Vec<String> = sqlx::query_scalar(
        "SELECT event_type FROM outbox_events
          WHERE workspace_id = $1
            AND aggregate_id IN (
                SELECT id FROM email_templates WHERE workspace_id = $1 AND name = 'outbox-updated'
                UNION ALL
                SELECT id FROM template_versions WHERE workspace_id = $1
                  AND template_id = (SELECT id FROM email_templates WHERE workspace_id = $1 AND name = 'outbox-updated')
            )
          ORDER BY event_type",
    )
    .bind(world.workspace_id)
    .fetch_all(tx.conn())
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(
        events,
        vec![
            "template.created".to_string(),
            "template.updated".to_string(),
            "template.version.created".to_string(),
        ]
    );
}
