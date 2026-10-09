//! Task 5 (imports) API tests — HTTP-level behavior only (the api crate
//! has no sqlx dep; DB-level assertions including T32 live in
//! worker/tests/imports_jobs.rs).
//!
//! - upload capability → PUT bytes → stored object → validate job →
//!   per-row statuses/errors → commit → export (+download).
//! - Commit requires the exact `validation_hash` (SNAPSHOT_MISMATCH).
//! - Formula-injection escaping verified on the downloaded CSV.
//! - `crm.import`/`crm.export` scope + bound upload token + Idempotency-Key.

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

/// upload-request → PUT bytes → returns stored object id.
async fn upload_csv(app: &TestApp, csrf: &str, csv: &str) -> Uuid {
    let res = app
        .post(
            "/api/v1/imports/upload-requests",
            json!({
                "filename": "contacts.csv",
                "media_type": "text/csv",
                "byte_size": csv.len()
            }),
            Some(csrf),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK, "upload-request failed");
    let data = TestApp::data(res).await;
    let res = app
        .client
        .put(app.url(&format!(
            "{}?token={}",
            data["upload_url"].as_str().unwrap(),
            data["upload_token"].as_str().unwrap()
        )))
        .header("x-csrf-token", csrf)
        .body(csv.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED, "upload failed");
    TestApp::data(res).await["object_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

/// POST /imports → 202 import job id (validation queued).
async fn create_import(app: &TestApp, csrf: &str, object_id: Uuid, strategy: &str) -> Uuid {
    let res = app
        .client
        .post(app.url("/api/v1/imports"))
        .header("x-csrf-token", csrf)
        .header("idempotency-key", format!("imp-{}", Uuid::new_v4()))
        .json(&json!({
            "stored_object_id": object_id,
            "mapping": {
                "columns": {
                    "Company": "company.name",
                    "Domain": "company.domain",
                    "Name": "contact.name",
                    "Email": "contact.email",
                    "Title": "contact.job_title"
                },
                "has_header": true,
                "duplicate_strategy": strategy
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED, "import create failed");
    TestApp::data(res).await["import_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

async fn seed_import_job_record(
    tx: &mut infrastructure::db::Tx<'_>,
    requested_by: Uuid,
    status: &str,
) -> Uuid {
    let key = Uuid::new_v4().simple().to_string();
    let object_id = infrastructure::repo::imports::insert_stored_object(
        tx,
        &key,
        "text/csv",
        1,
        &infrastructure::crypto::sha256_bytes(b"x"),
        "clean",
        Some(requested_by),
    )
    .await
    .unwrap();
    infrastructure::repo::imports::insert_import_job(
        tx,
        object_id,
        requested_by,
        &json!({"job_kind": "import", "columns": {}}),
        status,
    )
    .await
    .unwrap()
}

/// Drive the durable `imports.validate` / `imports.commit` jobs the same
/// way the worker handler does (application entry points, test tx).
async fn run_validate(db: &Db, ws: Uuid, import_id: Uuid) {
    let store = application::imports::object_store(&Config::test()).unwrap();
    let mut tx = db.begin(ws).await.unwrap();
    application::imports::validate::run(&mut tx, &store, import_id, None)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

async fn run_commit(db: &Db, ws: Uuid, import_id: Uuid) {
    let mut tx = db.begin(ws).await.unwrap();
    application::imports::commit_job::run(&mut tx, import_id, None)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

async fn get_import(app: &TestApp, id: Uuid) -> serde_json::Value {
    let res = app.get(&format!("/api/v1/imports/{id}")).await;
    assert_eq!(res.status(), StatusCode::OK);
    TestApp::data(res).await
}

async fn commit_request(app: &TestApp, csrf: &str, id: Uuid, hash: &str) -> reqwest::Response {
    app.client
        .post(app.url(&format!("/api/v1/imports/{id}/commit")))
        .header("x-csrf-token", csrf)
        .header("idempotency-key", format!("c-{}", Uuid::new_v4()))
        .json(&json!({
            "validation_hash": hash,
            "invalid_row_policy": "skip"
        }))
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn import_validate_commit_lifecycle() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let csrf = login_csrf(&app, world.admin.email.as_deref().unwrap()).await;
    let db = app.state.db.clone();
    let ws = world.workspace_id;

    // Seed an existing address so one CSV row validates as 'duplicate'.
    {
        let mut tx = db.begin(ws).await.unwrap();
        infrastructure::repo::research::find_or_create_email_address(
            &mut tx,
            "Dup@X.Test",
            "dup@x.test",
            1,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }

    let csv = "Company,Domain,Name,Email,Title\n\
               Acme,acme.test,Kim,kim@acme.test,VP\n\
               BadMail,bad.test,Bo,not-an-email,\n\
               Dup,dup.test,Dee,dup@x.test,\n\
               ,,,\n";
    let object_id = upload_csv(&app, &csrf, csv).await;
    let import_id = create_import(&app, &csrf, object_id, "link").await;

    // Missing Idempotency-Key → 400 (required extractor).
    let res = app
        .client
        .post(app.url("/api/v1/imports"))
        .header("x-csrf-token", &csrf)
        .json(&json!({"stored_object_id": object_id, "mapping": {"columns": {"Email": "contact.email"}}}))
        .send()
        .await
        .unwrap();
    TestApp::problem(res, StatusCode::BAD_REQUEST).await;

    run_validate(&db, ws, import_id).await;
    let job = get_import(&app, import_id).await;
    assert_eq!(job["status"], "ready");
    assert_eq!(job["summary"]["total_rows"], 4);
    assert_eq!(job["summary"]["valid_rows"], 1);
    assert_eq!(job["summary"]["invalid_rows"], 2);
    assert_eq!(job["summary"]["duplicate_rows"], 1);
    let hash = job["validation_hash"].as_str().unwrap().to_string();
    assert_eq!(hash.len(), 64);

    // Rows listing: per-row status + errors.
    let res = app.get(&format!("/api/v1/imports/{import_id}/rows")).await;
    let rows = TestApp::data(res).await;
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0]["status"], "valid");
    assert_eq!(rows[1]["status"], "invalid");
    assert_eq!(rows[1]["errors"][0]["code"], "invalid_email");
    assert_eq!(rows[2]["status"], "duplicate");
    assert_eq!(rows[3]["status"], "invalid");

    // Commit with wrong hash → 409 SNAPSHOT_MISMATCH.
    let res = commit_request(&app, &csrf, import_id, &"0".repeat(64)).await;
    let p = TestApp::problem(res, StatusCode::CONFLICT).await;
    assert_eq!(p["code"], "SNAPSHOT_MISMATCH");

    // Correct hash → 202 committing; drive the job; completed.
    let res = commit_request(&app, &csrf, import_id, &hash).await;
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    run_commit(&db, ws, import_id).await;
    let job = get_import(&app, import_id).await;
    assert_eq!(job["status"], "completed");
    assert_eq!(job["summary"]["imported_rows"], 2); // valid + linked duplicate

    // Rows now carry their imported links.
    let res = app
        .get(&format!("/api/v1/imports/{import_id}/rows?status=imported"))
        .await;
    let rows = TestApp::data(res).await;
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows[0]["contact_id"].is_string());
}

#[tokio::test]
async fn export_produces_formula_injection_safe_csv() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let csrf = login_csrf(&app, world.admin.email.as_deref().unwrap()).await;
    let db = app.state.db.clone();
    let ws = world.workspace_id;

    // Seed a dangerous company name via the repo layer (no sqlx dep here).
    {
        let mut tx = db.begin(ws).await.unwrap();
        infrastructure::repo::research::insert_company(
            &mut tx,
            "=1+cmd|evil",
            Some("evil.test"),
            None,
            None,
            None,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }

    let res = app
        .client
        .post(app.url("/api/v1/exports"))
        .header("x-csrf-token", &csrf)
        .header("idempotency-key", format!("exp-{}", Uuid::new_v4()))
        .json(&json!({
            "fields": ["company.name", "contact.name", "contact.email"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    let export_id: Uuid = TestApp::data(res).await["import_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    // Drive the export job.
    let store = application::imports::object_store(&Config::test()).unwrap();
    let mut tx = db.begin(ws).await.unwrap();
    application::imports::export_job::run(&mut tx, &store, export_id, None)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let job = get_import(&app, export_id).await;
    assert_eq!(job["status"], "completed");
    let download = job["download_url"].as_str().unwrap().to_string();

    let res = app.get(&download).await;
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.text().await.unwrap();
    let mut lines = body.lines();
    assert_eq!(
        lines.next().unwrap(),
        "company.name,contact.name,contact.email"
    );
    let row = lines.next().unwrap();
    assert!(
        row.starts_with("'=1+cmd|evil"),
        "formula cell must be quote-prefixed: {row}"
    );

    // Download requires the token AND session authz — a tokenless call is
    // rejected by the query extractor (axum's default 400, not a problem
    // body).
    let bare = download.split('?').next().unwrap().to_string();
    let res = app.get(&bare).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // Member lacks crm.export → 403 on POST /exports.
    let member_csrf = login_csrf(&app, world.member_a.email.as_deref().unwrap()).await;
    let res = app
        .client
        .post(app.url("/api/v1/exports"))
        .header("x-csrf-token", &member_csrf)
        .header("idempotency-key", format!("exp-{}", Uuid::new_v4()))
        .json(&json!({"fields": ["company.name"]}))
        .send()
        .await
        .unwrap();
    TestApp::problem(res, StatusCode::FORBIDDEN).await;
}

#[tokio::test]
async fn upload_request_size_and_token_rules() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let csrf = login_csrf(&app, world.admin.email.as_deref().unwrap()).await;

    // >20MB declared → 422.
    let res = app
        .post(
            "/api/v1/imports/upload-requests",
            json!({
                "filename": "big.csv",
                "media_type": "text/csv",
                "byte_size": 20 * 1024 * 1024 + 1
            }),
            Some(&csrf),
        )
        .await;
    TestApp::problem(res, StatusCode::UNPROCESSABLE_ENTITY).await;

    // Wrong media type → 422.
    let res = app
        .post(
            "/api/v1/imports/upload-requests",
            json!({
                "filename": "x.png", "media_type": "image/png", "byte_size": 10
            }),
            Some(&csrf),
        )
        .await;
    TestApp::problem(res, StatusCode::UNPROCESSABLE_ENTITY).await;

    // Forged/absent token on PUT → 403.
    let res = app
        .client
        .put(app.url("/api/v1/imports/uploads/deadbeef?token=bogus"))
        .header("x-csrf-token", &csrf)
        .body("a,b")
        .send()
        .await
        .unwrap();
    TestApp::problem(res, StatusCode::FORBIDDEN).await;

    // Member lacks crm.import → 403 on upload-request.
    let member_csrf = login_csrf(&app, world.member_a.email.as_deref().unwrap()).await;
    let res = app
        .post(
            "/api/v1/imports/upload-requests",
            json!({"filename": "c.csv", "media_type": "text/csv", "byte_size": 10}),
            Some(&member_csrf),
        )
        .await;
    TestApp::problem(res, StatusCode::FORBIDDEN).await;
}

#[tokio::test]
async fn imports_list_is_cursor_paged_scoped_and_validates_status() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let _csrf = login_csrf(&app, world.admin.email.as_deref().unwrap()).await;

    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let _admin_job = seed_import_job_record(&mut tx, world.admin.principal_id, "validating").await;
    let agent_job_a = seed_import_job_record(&mut tx, world.agent.principal_id, "validating").await;
    let agent_job_b = seed_import_job_record(&mut tx, world.agent.principal_id, "ready").await;
    let (_credential_id, agent_token) = infrastructure::repo::auth::create_service_credential(
        &mut tx,
        world.agent.principal_id,
        "imports-list-test",
        &["crm.import".to_string()],
        chrono::Duration::hours(1),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    // An agent with import scope sees only its own jobs, with a principal-
    // bound cursor across the two owned rows.
    let first = app
        .client
        .get(app.url("/api/v1/imports?limit=1"))
        .header("authorization", format!("Bearer {agent_token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let first_body: serde_json::Value = first.json().await.unwrap();
    assert_eq!(first_body["data"].as_array().unwrap().len(), 1);
    assert_eq!(first_body["page"]["has_more"], true);
    let first_id = first_body["data"][0]["id"].as_str().unwrap().to_string();
    assert!(
        [agent_job_a.to_string(), agent_job_b.to_string()].contains(&first_id),
        "agent received a job owned by another principal: {first_body}"
    );
    assert_eq!(
        first_body["data"][0]["requested_by"],
        world.agent.principal_id.to_string()
    );
    let cursor = first_body["page"]["next_cursor"].as_str().unwrap();
    let second = app
        .client
        .get(app.url(&format!("/api/v1/imports?limit=1&cursor={cursor}")))
        .header("authorization", format!("Bearer {agent_token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::OK);
    let second_body: serde_json::Value = second.json().await.unwrap();
    assert_eq!(second_body["data"].as_array().unwrap().len(), 1);
    assert_ne!(second_body["data"][0]["id"], first_id);
    assert_eq!(
        second_body["data"][0]["requested_by"],
        world.agent.principal_id.to_string()
    );

    let invalid = app
        .client
        .get(app.url("/api/v1/imports?status=not-a-status"))
        .header("authorization", format!("Bearer {agent_token}"))
        .send()
        .await
        .unwrap();
    let problem = TestApp::problem(invalid, StatusCode::UNPROCESSABLE_ENTITY).await;
    assert_eq!(problem["code"], "VALIDATION_FAILED");

    // Human Admin sees the workspace jobs, including both agent-owned rows.
    let all = app.get("/api/v1/imports?limit=10").await;
    assert_eq!(all.status(), StatusCode::OK);
    let all_body: serde_json::Value = all.json().await.unwrap();
    let visible = all_body["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|job| job["id"].as_str())
        .collect::<Vec<_>>();
    let agent_job_a = agent_job_a.to_string();
    let agent_job_b = agent_job_b.to_string();
    assert!(visible.contains(&agent_job_a.as_str()));
    assert!(visible.contains(&agent_job_b.as_str()));
}
