//! Task 4 (crm) HTTP tests: T01 company isolation, B.3.3 create/duplicates,
//! If-Match concurrency (428/412), idempotent create replay, merge-requests
//! authz + replay, member ownership rules, B.3.4 email linking + T31
//! groundwork, notes mailbox ACL, tasks member scoping, cursor pagination.

use api::AppState;
use api::testing::TestApp;
use infrastructure::config::Config;
use infrastructure::db::{Db, ROLE_API, ROLE_RETENTION};
use infrastructure::retention::{CacheInvalidator, ErasureRequest, erase_contact};
use infrastructure::storage::ObjectStore;
use infrastructure::testing::{TEST_PASSWORD, TestWorld, ctx_for_credential, seed_workspace};
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

#[tokio::test]
async fn contact_create_with_contact_details_is_atomic_and_replayable() {
    let Some((db, world)) = setup().await else {
        return;
    };
    let app = app_for(&db, &world).await;
    let csrf = login(&app, world.admin.email.as_deref().unwrap()).await;
    let body =
        json!({"name":"Quick contact","phone":"+82 10-1234-5678","email":"quick@example.test"});
    let response = post(
        &app,
        "/api/v1/contacts",
        body.clone(),
        &csrf,
        Some("contact-details-create-001"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let saved = TestApp::data(response).await;
    assert_eq!(saved["phone"], "+82 10-1234-5678");
    assert_eq!(saved["emails"][0]["address"], "quick@example.test");
    assert_eq!(saved["emails"][0]["is_primary"], true);
    let replay = TestApp::data(
        post(
            &app,
            "/api/v1/contacts",
            body,
            &csrf,
            Some("contact-details-create-001"),
        )
        .await,
    )
    .await;
    assert_eq!(replay["id"], saved["id"]);
    let path = format!("/api/v1/contacts/{}", saved["id"].as_str().unwrap());
    let kept = app
        .client
        .patch(app.url(&path))
        .header("x-csrf-token", &csrf)
        .header(
            "if-match",
            format!("\"{}\"", saved["version"].as_i64().unwrap()),
        )
        .json(&json!({"job_title":"Director"}))
        .send()
        .await
        .unwrap();
    assert_eq!(kept.status(), StatusCode::OK);
    let kept = TestApp::data(kept).await;
    assert_eq!(kept["phone"], saved["phone"]);
    let cleared = app
        .client
        .patch(app.url(&path))
        .header("x-csrf-token", &csrf)
        .header(
            "if-match",
            format!("\"{}\"", kept["version"].as_i64().unwrap()),
        )
        .json(&json!({"phone":null}))
        .send()
        .await
        .unwrap();
    assert_eq!(cleared.status(), StatusCode::OK);
    assert!(TestApp::data(cleared).await["phone"].is_null());
    let invalid = post(
        &app,
        "/api/v1/contacts",
        json!({"name":"Atomic invalid contact","email":"invalid-address","phone":"010-1234-5678"}),
        &csrf,
        None,
    )
    .await;
    assert!(invalid.status().is_client_error());
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM contacts WHERE name='Atomic invalid contact'")
            .fetch_one(tx.conn())
            .await
            .unwrap();
    assert_eq!(count, 0);
    tx.commit().await.unwrap();
}

async fn setup() -> Option<(Db, TestWorld)> {
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
    Some((db, world))
}

async fn app_for(db: &Db, world: &TestWorld) -> TestApp {
    let db = db.clone().with_default_workspace(Some(world.workspace_id));
    TestApp::spawn_with_state(AppState::with_fake_providers(
        db,
        Arc::new(Config::test()),
        None,
    ))
    .await
}

/// Login helper returning the CSRF token for subsequent mutations.
async fn login(app: &TestApp, email: &str) -> String {
    let res = app.login(email, TEST_PASSWORD).await;
    assert!(res.status().is_success(), "login failed for {email}");
    TestApp::csrf_token_from(&res).expect("no csrf cookie")
}

async fn post(
    app: &TestApp,
    path: &str,
    body: Value,
    csrf: &str,
    idem: Option<&str>,
) -> reqwest::Response {
    let mut req = app
        .client
        .post(app.url(path))
        .header("x-csrf-token", csrf)
        .json(&body);
    if let Some(k) = idem {
        req = req.header("idempotency-key", k);
    }
    req.send().await.unwrap()
}

async fn patch(
    app: &TestApp,
    path: &str,
    body: Value,
    csrf: &str,
    if_match: Option<i64>,
) -> reqwest::Response {
    let mut req = app
        .client
        .patch(app.url(path))
        .header("x-csrf-token", csrf)
        .json(&body);
    if let Some(v) = if_match {
        req = req.header("if-match", format!("\"{v}\""));
    }
    req.send().await.unwrap()
}

async fn create_company(app: &TestApp, csrf: &str, name: &str, domain: Option<&str>) -> Value {
    let mut body = json!({"name": name});
    if let Some(d) = domain {
        body["domain"] = json!(d);
    }
    let res = post(
        app,
        "/api/v1/companies",
        body,
        csrf,
        Some(&Uuid::new_v4().to_string()),
    )
    .await;
    TestApp::data(res).await
}

// ------------------------------------------------------------------ T01

/// T01: a company from another workspace is invisible — 404, not 403.
#[tokio::test]
async fn t01_company_other_workspace_404() {
    let Some((db, world_a)) = setup().await else {
        return;
    };
    let world_b = seed_workspace(&db).await.unwrap();
    let app_a = app_for(&db, &world_a).await;
    let app_b = app_for(&db, &world_b).await;

    let csrf_a = login(&app_a, world_a.admin.email.as_deref().unwrap()).await;
    let company = create_company(&app_a, &csrf_a, "Acme Isolation", Some("acme-iso.dev")).await;
    let id = company["id"].as_str().unwrap();

    let csrf_b = login(&app_b, world_b.admin.email.as_deref().unwrap()).await;
    let res = app_b.get(&format!("/api/v1/companies/{id}")).await;
    let problem = TestApp::problem(res, StatusCode::NOT_FOUND).await;
    assert_eq!(problem["code"], "NOT_FOUND");

    // List and activity timeline hide it too.
    let res = app_b.get("/api/v1/companies").await;
    let data = TestApp::data(res).await;
    assert!(data.as_array().unwrap().iter().all(|c| c["id"] != *id));
    let res = app_b
        .get(&format!("/api/v1/companies/{id}/activities"))
        .await;
    TestApp::problem(res, StatusCode::NOT_FOUND).await;
    // Mutations hidden the same way.
    let res = patch(
        &app_b,
        &format!("/api/v1/companies/{id}"),
        json!({"name": "x"}),
        &csrf_b,
        Some(1),
    )
    .await;
    TestApp::problem(res, StatusCode::NOT_FOUND).await;
}

// ------------------------------------------------- companies write flows

/// B.3.3 create + advisory duplicates; If-Match lifecycle on patch/archive.
#[tokio::test]
async fn company_create_patch_archive_flow() {
    let Some((db, world)) = setup().await else {
        return;
    };
    let app = app_for(&db, &world).await;
    let csrf = login(&app, world.admin.email.as_deref().unwrap()).await;

    let first = create_company(&app, &csrf, "Dup Corp", Some("dup.example")).await;
    let second = create_company(&app, &csrf, "Dup Corp Two", Some("DUP.example")).await;
    let dupes = second["duplicate_candidates"].as_array().unwrap();
    assert!(
        dupes.iter().any(|d| d["id"] == first["id"]
            && d["matched_on"]
                .as_array()
                .unwrap()
                .contains(&json!("domain"))),
        "expected domain duplicate candidate: {dupes:?}"
    );

    let id = second["id"].as_str().unwrap().to_string();
    // GET returns ETag.
    let res = app.get(&format!("/api/v1/companies/{id}")).await;
    let etag = res
        .headers()
        .get("etag")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(etag, "\"1\"");

    // PATCH without If-Match → 428; stale → 412; correct → 200 + new ETag.
    let res = patch(
        &app,
        &format!("/api/v1/companies/{id}"),
        json!({"name": "Renamed"}),
        &csrf,
        None,
    )
    .await;
    let p = TestApp::problem(res, StatusCode::PRECONDITION_REQUIRED).await;
    assert_eq!(p["code"], "VERSION_PRECONDITION_REQUIRED");
    let res = patch(
        &app,
        &format!("/api/v1/companies/{id}"),
        json!({"name": "Renamed"}),
        &csrf,
        Some(9),
    )
    .await;
    let p = TestApp::problem(res, StatusCode::PRECONDITION_FAILED).await;
    assert_eq!(p["code"], "VERSION_MISMATCH");
    let res = patch(
        &app,
        &format!("/api/v1/companies/{id}"),
        json!({"name": "Renamed", "domain": null}),
        &csrf,
        Some(1),
    )
    .await;
    let updated = TestApp::data(res).await;
    assert_eq!(updated["name"], "Renamed");
    assert_eq!(updated["domain"], Value::Null);
    assert_eq!(updated["version"], 2);

    // owner_principal_id is not patchable (A.6.1 separate command).
    let res = patch(
        &app,
        &format!("/api/v1/companies/{id}"),
        json!({"owner_principal_id": world.admin.principal_id}),
        &csrf,
        Some(2),
    )
    .await;
    TestApp::problem(res, StatusCode::BAD_REQUEST).await;

    // Archive requires If-Match; second archive → 409.
    let res = post(
        &app,
        &format!("/api/v1/companies/{id}/archive"),
        json!({}),
        &csrf,
        None,
    )
    .await;
    let p = TestApp::problem(res, StatusCode::PRECONDITION_REQUIRED).await;
    assert_eq!(p["code"], "VERSION_PRECONDITION_REQUIRED");
    let res = app
        .client
        .post(app.url(&format!("/api/v1/companies/{id}/archive")))
        .header("x-csrf-token", &csrf)
        .header("if-match", "\"2\"")
        .json(&json!({"reason": "cleanup"}))
        .send()
        .await
        .unwrap();
    let archived = TestApp::data(res).await;
    assert!(archived["archived_at"].is_string());
    assert_eq!(archived["version"], 3);

    // Archived rows only surface under ?archived=true.
    let res = app.get("/api/v1/companies").await;
    let data = TestApp::data(res).await;
    assert!(data.as_array().unwrap().iter().all(|c| c["id"] != id));
    let res = app.get("/api/v1/companies?archived=true").await;
    let data = TestApp::data(res).await;
    assert!(data.as_array().unwrap().iter().any(|c| c["id"] == id));
}

/// Idempotency: same key + same body replays; same key + different body 409.
#[tokio::test]
async fn company_create_idempotent_replay() {
    let Some((db, world)) = setup().await else {
        return;
    };
    let app = app_for(&db, &world).await;
    let csrf = login(&app, world.admin.email.as_deref().unwrap()).await;
    let key = Uuid::new_v4().to_string();
    let body = json!({"name": "Idem Corp", "domain": "idem.example"});

    let res = post(&app, "/api/v1/companies", body.clone(), &csrf, Some(&key)).await;
    let first = TestApp::data(res).await;
    let res = post(&app, "/api/v1/companies", body, &csrf, Some(&key)).await;
    let replayed = TestApp::data(res).await;
    assert_eq!(first["id"], replayed["id"], "replay returned a new company");

    let res = post(
        &app,
        "/api/v1/companies",
        json!({"name": "Different Corp"}),
        &csrf,
        Some(&key),
    )
    .await;
    let p = TestApp::problem(res, StatusCode::CONFLICT).await;
    assert_eq!(p["code"], "IDEMPOTENCY_KEY_REUSED");
}

/// CRM writes persist their versioned event in the same workspace transaction.
#[tokio::test]
async fn company_create_emits_versioned_outbox_event() {
    let Some((db, world)) = setup().await else {
        return;
    };
    let app = app_for(&db, &world).await;
    let csrf = login(&app, world.admin.email.as_deref().unwrap()).await;
    let company = create_company(&app, &csrf, "Outbox Corp", Some("outbox.example")).await;
    let company_id = Uuid::parse_str(company["id"].as_str().unwrap()).unwrap();

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let events = infrastructure::outbox::claim_unpublished(&mut tx, 100, &[])
        .await
        .unwrap();
    let event = events
        .into_iter()
        .find(|event| event.event_type == "company.created" && event.aggregate_id == company_id)
        .expect("company create must have an outbox event");
    tx.commit().await.unwrap();

    assert_eq!(event.aggregate_type, "company");
    assert_eq!(event.aggregate_version, 1);
    assert_eq!(event.payload, json!({"id": company_id, "version": 1}));
}

/// Merge-requests: Manager/Admin only (§5.3), 202 + job, idempotent replay.
#[tokio::test]
async fn merge_request_authz_and_replay() {
    let Some((db, world)) = setup().await else {
        return;
    };
    let app = app_for(&db, &world).await;
    let csrf_admin = login(&app, world.admin.email.as_deref().unwrap()).await;
    let src = create_company(&app, &csrf_admin, "Merge Source", Some("src.example")).await;
    let tgt = create_company(&app, &csrf_admin, "Merge Target", Some("tgt.example")).await;
    let src_id = src["id"].as_str().unwrap();
    let tgt_id = tgt["id"].as_str().unwrap();

    // Member cannot merge.
    let csrf_member = login(&app, world.member_a.email.as_deref().unwrap()).await;
    let res = post(
        &app,
        &format!("/api/v1/companies/{tgt_id}/merge-requests"),
        json!({"source_id": src_id}),
        &csrf_member,
        Some(&Uuid::new_v4().to_string()),
    )
    .await;
    TestApp::problem(res, StatusCode::FORBIDDEN).await;

    // Self-merge and bad field_choices → 400.
    let csrf_mgr = login(&app, world.manager.email.as_deref().unwrap()).await;
    let res = post(
        &app,
        &format!("/api/v1/companies/{tgt_id}/merge-requests"),
        json!({"source_id": tgt_id}),
        &csrf_mgr,
        Some(&Uuid::new_v4().to_string()),
    )
    .await;
    TestApp::problem(res, StatusCode::BAD_REQUEST).await;
    let res = post(
        &app,
        &format!("/api/v1/companies/{tgt_id}/merge-requests"),
        json!({"source_id": src_id, "field_choices": {"version": "source"}}),
        &csrf_mgr,
        Some(&Uuid::new_v4().to_string()),
    )
    .await;
    TestApp::problem(res, StatusCode::BAD_REQUEST).await;

    // Manager request → 202 + queued job; replay returns the same job.
    let key = Uuid::new_v4().to_string();
    let body = json!({"source_id": src_id, "field_choices": {"name": "source"}});
    let res = post(
        &app,
        &format!("/api/v1/companies/{tgt_id}/merge-requests"),
        body.clone(),
        &csrf_mgr,
        Some(&key),
    )
    .await;
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    let first = TestApp::data(res).await;
    assert_eq!(first["status"], "queued");
    let res = post(
        &app,
        &format!("/api/v1/companies/{tgt_id}/merge-requests"),
        body,
        &csrf_mgr,
        Some(&key),
    )
    .await;
    let second = TestApp::data(res).await;
    assert_eq!(first["job_id"], second["job_id"]);
}

/// Member ownership rules (§5.3): own/unassigned editable; reassign denied.
#[tokio::test]
async fn member_ownership_rules() {
    let Some((db, world)) = setup().await else {
        return;
    };
    let app = app_for(&db, &world).await;
    let csrf_a = login(&app, world.member_a.email.as_deref().unwrap()).await;

    // Member assigning a different owner → 403.
    let res = post(
        &app,
        "/api/v1/companies",
        json!({"name": "Owned Elsewhere", "owner_principal_id": world.admin.principal_id}),
        &csrf_a,
        Some(&Uuid::new_v4().to_string()),
    )
    .await;
    TestApp::problem(res, StatusCode::FORBIDDEN).await;

    // Member creates with self owner.
    let company = {
        let res = post(
            &app,
            "/api/v1/companies",
            json!({"name": "Member Corp", "owner_principal_id": world.member_a.principal_id}),
            &csrf_a,
            Some(&Uuid::new_v4().to_string()),
        )
        .await;
        assert_eq!(res.status(), StatusCode::CREATED);
        TestApp::data(res).await
    };
    let id = company["id"].as_str().unwrap().to_string();

    // Member B cannot patch member A's company; member A can.
    let csrf_b = login(&app, world.member_b.email.as_deref().unwrap()).await;
    let res = patch(
        &app,
        &format!("/api/v1/companies/{id}"),
        json!({"name": "Hijack"}),
        &csrf_b,
        Some(1),
    )
    .await;
    TestApp::problem(res, StatusCode::FORBIDDEN).await;
    let csrf_a = login(&app, world.member_a.email.as_deref().unwrap()).await;
    let res = patch(
        &app,
        &format!("/api/v1/companies/{id}"),
        json!({"name": "Mine"}),
        &csrf_a,
        Some(1),
    )
    .await;
    assert!(res.status().is_success());
}

// ------------------------------------------------------------------ T31

/// T31 groundwork: one normalized address linked to 3 contacts stays a
/// single `email_addresses` row (B.3.4, §6.3).
#[tokio::test]
async fn t31_shared_address_single_row() {
    let Some((db, world)) = setup().await else {
        return;
    };
    let app = app_for(&db, &world).await;
    let csrf = login(&app, world.admin.email.as_deref().unwrap()).await;

    let mut address_ids = Vec::new();
    let mut contact_ids = Vec::new();
    for name in ["Alice", "Bob", "Carol"] {
        let res = post(&app, "/api/v1/contacts", json!({"name": name}), &csrf, None).await;
        let c = TestApp::data(res).await;
        contact_ids.push(c["id"].as_str().unwrap().to_string());
    }
    for (i, cid) in contact_ids.iter().enumerate() {
        let res = post(
            &app,
            &format!("/api/v1/contacts/{cid}/emails"),
            json!({"address": "Sales@Test.dev", "is_primary": i == 0}),
            &csrf,
            None,
        )
        .await;
        let e = TestApp::data(res).await;
        address_ids.push(e["email_address_id"].as_str().unwrap().to_string());
        // B.3.4: eligibility preview present, shared links surface after 2nd.
        assert!(
            e["marketing_eligibility"]["reason_codes"]
                .as_array()
                .unwrap()
                .contains(&json!("SEND_BASIS_UNVERIFIED"))
        );
        if i == 2 {
            assert_eq!(e["linked_contact_ids"].as_array().unwrap().len(), 2);
        }
    }
    assert!(
        address_ids.iter().all(|id| id == &address_ids[0]),
        "shared address produced multiple email_addresses rows"
    );
    // (The single-row DB assertion lives in worker/tests/crm_jobs.rs —
    //  the api crate has no sqlx dev-dep.)
}

/// Primary-email switch + link removal (DELETE promotes the next link).
#[tokio::test]
async fn contact_primary_email_and_remove() {
    let Some((db, world)) = setup().await else {
        return;
    };
    let app = app_for(&db, &world).await;
    let csrf = login(&app, world.admin.email.as_deref().unwrap()).await;

    let res = post(
        &app,
        "/api/v1/contacts",
        json!({"name": "Prim"}),
        &csrf,
        None,
    )
    .await;
    let contact = TestApp::data(res).await;
    let cid = contact["id"].as_str().unwrap().to_string();

    let mut ids = Vec::new();
    for (addr, primary) in [("one@test.dev", true), ("two@test.dev", false)] {
        let res = post(
            &app,
            &format!("/api/v1/contacts/{cid}/emails"),
            json!({"address": addr, "is_primary": primary}),
            &csrf,
            None,
        )
        .await;
        let e = TestApp::data(res).await;
        ids.push(e["email_address_id"].as_str().unwrap().to_string());
    }

    // PUT primary-email without If-Match → 428; stale → 412; ok → switched.
    let res = app
        .client
        .put(app.url(&format!("/api/v1/contacts/{cid}/primary-email")))
        .header("x-csrf-token", &csrf)
        .json(&json!({"email_address_id": ids[1]}))
        .send()
        .await
        .unwrap();
    TestApp::problem(res, StatusCode::PRECONDITION_REQUIRED).await;
    let res = app
        .client
        .put(app.url(&format!("/api/v1/contacts/{cid}/primary-email")))
        .header("x-csrf-token", &csrf)
        .header("if-match", "\"99\"")
        .json(&json!({"email_address_id": ids[1]}))
        .send()
        .await
        .unwrap();
    TestApp::problem(res, StatusCode::PRECONDITION_FAILED).await;
    // Version was bumped twice by the two link inserts → current is 3.
    let res = app.get(&format!("/api/v1/contacts/{cid}")).await;
    let cur = TestApp::data(res).await;
    let version = cur["version"].as_i64().unwrap();
    let res = app
        .client
        .put(app.url(&format!("/api/v1/contacts/{cid}/primary-email")))
        .header("x-csrf-token", &csrf)
        .header("if-match", format!("\"{version}\""))
        .json(&json!({"email_address_id": ids[1]}))
        .send()
        .await
        .unwrap();
    let updated = TestApp::data(res).await;
    let primary = updated["emails"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["is_primary"] == true)
        .unwrap();
    assert_eq!(primary["email_address_id"], ids[1]);

    // DELETE the primary link → the remaining link is promoted.
    let res = app
        .client
        .delete(app.url(&format!("/api/v1/contacts/{cid}/emails/{}", ids[1])))
        .header("x-csrf-token", &csrf)
        .header("if-match", format!("\"{}\"", updated["version"]))
        .send()
        .await
        .unwrap();
    let after = TestApp::data(res).await;
    let emails = after["emails"].as_array().unwrap();
    assert_eq!(emails.len(), 1);
    assert_eq!(emails[0]["email_address_id"], ids[0]);
    assert_eq!(emails[0]["is_primary"], true);
}

// ------------------------------------------------------------------- notes

/// Notes: exactly-one-target 400; thread notes need mailbox read (§5.6).
#[tokio::test]
async fn notes_target_and_mailbox_acl() {
    let Some((db, world)) = setup().await else {
        return;
    };
    let app = app_for(&db, &world).await;
    let csrf = login(&app, world.admin.email.as_deref().unwrap()).await;

    let company = create_company(&app, &csrf, "Note Corp", None).await;
    let cid = company["id"].as_str().unwrap().to_string();

    // Zero / two targets → 400.
    let res = post(&app, "/api/v1/notes", json!({"body": "none"}), &csrf, None).await;
    TestApp::problem(res, StatusCode::BAD_REQUEST).await;
    let res = post(
        &app,
        "/api/v1/notes",
        json!({"body": "both", "company_id": cid, "contact_id": Uuid::new_v4()}),
        &csrf,
        None,
    )
    .await;
    TestApp::problem(res, StatusCode::BAD_REQUEST).await;

    // Company note → 201.
    let res = post(
        &app,
        "/api/v1/notes",
        json!({"company_id": cid, "body": "called them"}),
        &csrf,
        None,
    )
    .await;
    let note = TestApp::data(res).await;
    assert_eq!(note["company_id"], cid);
    assert_eq!(note["created_by"], json!(world.admin.principal_id));

    // (Thread-linked note ACL is covered in worker/tests/crm_jobs.rs —
    //  creating an email_threads fixture needs sqlx, which the api crate
    //  does not dev-depend on.)
}

struct NoopErasureObjectStore;

#[async_trait::async_trait]
impl ObjectStore for NoopErasureObjectStore {
    async fn put(&self, _key: &str, _plaintext: &[u8]) -> Result<(), contracts::AppError> {
        Ok(())
    }

    async fn get(&self, _key: &str) -> Result<Vec<u8>, contracts::AppError> {
        Err(contracts::AppError::not_found("object not found"))
    }

    async fn delete(&self, _key: &str) -> Result<(), contracts::AppError> {
        Ok(())
    }

    fn sign_access(&self, key: &str, _ttl: std::time::Duration) -> String {
        key.to_string()
    }

    fn verify_access(&self, key: &str, token: &str) -> bool {
        key == token
    }
}

struct NoopErasureCache;

#[async_trait::async_trait]
impl CacheInvalidator for NoopErasureCache {
    async fn invalidate_contact(
        &self,
        _workspace_id: Uuid,
        _contact_id: Uuid,
        _email_address_ids: &[Uuid],
    ) -> Result<(), contracts::AppError> {
        Ok(())
    }
}

#[tokio::test]
async fn company_activity_note_body_is_current_permission_filtered_and_removed_with_note() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping activity note-body test");
            return;
        }
    };
    let database_name = database_url
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .split('?')
        .next()
        .unwrap_or_default();
    assert_eq!(
        database_name, "outreach_sales_kanban_api",
        "activity note-body regression must use the dedicated API test database"
    );
    let Some((db, world)) = setup().await else {
        return;
    };
    let app = app_for(&db, &world).await;
    let csrf = login(&app, world.admin.email.as_deref().unwrap()).await;
    let company = create_company(&app, &csrf, "Note projection company", None).await;
    let company_id = Uuid::parse_str(company["id"].as_str().unwrap()).unwrap();
    let contact = TestApp::data(
        post(
            &app,
            "/api/v1/contacts",
            json!({"company_id":company_id,"name":"Note projection contact"}),
            &csrf,
            None,
        )
        .await,
    )
    .await;
    let contact_id = Uuid::parse_str(contact["id"].as_str().unwrap()).unwrap();

    let first_note = TestApp::data(
        post(
            &app,
            "/api/v1/notes",
            json!({"contact_id":contact_id,"body":"Original research note body"}),
            &csrf,
            None,
        )
        .await,
    )
    .await;
    let first_note_id = Uuid::parse_str(first_note["id"].as_str().unwrap()).unwrap();
    let current_body = "Updated current research note body";
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    sqlx::query(
        "UPDATE notes SET body = $3, version = version + 1, updated_at = now()
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(world.workspace_id)
    .bind(first_note_id)
    .bind(current_body)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let erased_body = "Contact research body to erase";
    let second_note = TestApp::data(
        post(
            &app,
            "/api/v1/notes",
            json!({"contact_id":contact_id,"body":erased_body}),
            &csrf,
            None,
        )
        .await,
    )
    .await;
    let second_note_id = Uuid::parse_str(second_note["id"].as_str().unwrap()).unwrap();

    // A mailbox-derived activity can carry an arbitrary note_id in its
    // summary; it must neither inherit CRM note content nor bypass can_read.
    let mail_activity_id: Uuid = sqlx::query_scalar(
        "INSERT INTO activities
            (workspace_id,company_id,contact_id,mail_account_id,kind,source_event_id,
             occurred_at,summary)
         VALUES ($1,NULL,$2,$3,'mail.received',$4,now(),$5) RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(contact_id)
    .bind(world.mail_account_id)
    .bind(Uuid::new_v4())
    .bind(json!({"note_id":second_note_id,"label":"mail activity"}))
    .fetch_one(db.pool())
    .await
    .unwrap();
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    sqlx::query(
        "UPDATE mail_account_grants
            SET can_read = false, can_compose = false, can_send = false,
                can_approve = false, can_assign = false, version = version + 1
          WHERE workspace_id = $1 AND mail_account_id = $2 AND principal_id = $3",
    )
    .bind(world.workspace_id)
    .bind(world.mail_account_id)
    .bind(world.agent.principal_id)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let activity_path = format!("/api/v1/companies/{company_id}/activities");
    let rows = TestApp::data(app.get(&activity_path).await).await;
    let first_activity = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| {
            row["kind"] == "note.created" && row["summary"]["note_id"] == first_note_id.to_string()
        })
        .unwrap();
    assert_eq!(first_activity["summary"]["body"], current_body);
    let second_activity = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| {
            row["kind"] == "note.created" && row["summary"]["note_id"] == second_note_id.to_string()
        })
        .unwrap();
    assert_eq!(second_activity["summary"]["body"], erased_body);
    let mail_activity = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == mail_activity_id.to_string())
        .unwrap();
    assert!(mail_activity["summary"].get("body").is_none());

    let agent_token = {
        let mut tx = db.begin(world.workspace_id).await.unwrap();
        let (_, token) = infrastructure::repo::auth::create_service_credential(
            &mut tx,
            world.agent.principal_id,
            "activity note read",
            &["crm.read".to_string()],
            chrono::Duration::hours(1),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        token
    };
    let agent_rows = app
        .client
        .get(app.url(&activity_path))
        .bearer_auth(&agent_token)
        .send()
        .await
        .unwrap();
    assert_eq!(agent_rows.status(), StatusCode::OK);
    let agent_rows = TestApp::data(agent_rows).await;
    assert!(agent_rows.as_array().unwrap().iter().any(|row| {
        row["kind"] == "note.created"
            && row["summary"]["note_id"] == first_note_id.to_string()
            && row["summary"]["body"] == current_body
    }));
    assert!(
        !agent_rows
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["id"] == mail_activity_id.to_string())
    );

    let no_crm_token = {
        let mut tx = db.begin(world.workspace_id).await.unwrap();
        let (_, token) = infrastructure::repo::auth::create_service_credential(
            &mut tx,
            world.agent.principal_id,
            "activity note no crm read",
            &[],
            chrono::Duration::hours(1),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        token
    };
    let denied = app
        .client
        .get(app.url(&activity_path))
        .bearer_auth(&no_crm_token)
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    let denied_body = denied.text().await.unwrap();
    assert!(!denied_body.contains(current_body));
    assert!(!denied_body.contains(erased_body));

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    sqlx::query("DELETE FROM notes WHERE workspace_id = $1 AND id = $2")
        .bind(world.workspace_id)
        .bind(first_note_id)
        .execute(tx.conn())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let after_delete = TestApp::data(app.get(&activity_path).await).await;
    let deleted_activity = after_delete
        .as_array()
        .unwrap()
        .iter()
        .find(|row| {
            row["kind"] == "note.created" && row["summary"]["note_id"] == first_note_id.to_string()
        })
        .unwrap();
    assert!(deleted_activity["summary"].get("body").is_none());

    let owner_db = Db::connect(&database_url, None).await.unwrap();
    let mut tx = owner_db.begin(world.workspace_id).await.unwrap();
    sqlx::query(
        "UPDATE activities
            SET summary = summary || jsonb_build_object('body', $3)
          WHERE workspace_id = $1 AND kind = 'note.created' AND source_event_id = $2",
    )
    .bind(world.workspace_id)
    .bind(first_note_id)
    .bind("stale note body that must stay hidden")
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let after_stale_summary = TestApp::data(app.get(&activity_path).await).await;
    let stale_activity = after_stale_summary
        .as_array()
        .unwrap()
        .iter()
        .find(|row| {
            row["kind"] == "note.created" && row["summary"]["note_id"] == first_note_id.to_string()
        })
        .unwrap();
    assert!(stale_activity["summary"].get("body").is_none());

    let retention_db = Db::connect(&database_url, Some(ROLE_RETENTION))
        .await
        .unwrap();
    erase_contact(
        &retention_db,
        ErasureRequest {
            workspace_id: world.workspace_id,
            contact_id,
            actor_principal_id: Some(world.admin.principal_id),
            request_id: "crm-note-body-erasure-test".into(),
            trace_id: None,
        },
        &NoopErasureObjectStore,
        &NoopErasureCache,
    )
    .await
    .unwrap();
    let after_erasure = TestApp::data(app.get(&activity_path).await).await;
    assert!(!after_erasure.as_array().unwrap().iter().any(|row| {
        row["summary"]["body"] == erased_body
            || row["summary"]["note_id"] == second_note_id.to_string()
    }));
}

/// MCP CRM notes are a scoped internal-memo action; the agent may add a note
/// without gaining permission to patch the company itself.
#[tokio::test]
async fn agent_with_crm_write_can_create_internal_note() {
    let Some((db, world)) = setup().await else {
        return;
    };
    let app = app_for(&db, &world).await;
    let csrf = login(&app, world.admin.email.as_deref().unwrap()).await;
    let company = create_company(&app, &csrf, "Agent Note Target", None).await;

    let ctx = ctx_for_credential(
        &world,
        &world.agent,
        Uuid::new_v4(),
        contracts::ScopeSet::of([contracts::Scope::CrmWrite]),
        "crm-agent-note",
    );
    let created = application::crm::notes::create(
        &db,
        &ctx,
        &contracts::crm::CreateNoteRequest {
            company_id: Some(Uuid::parse_str(company["id"].as_str().unwrap()).unwrap()),
            contact_id: None,
            thread_id: None,
            body: "Source indicates a recent expansion.".into(),
        },
        Some("agent-note-once-1".into()),
    )
    .await
    .expect("crm.write agents may add internal notes");

    assert_eq!(created.body.created_by, world.agent.principal_id);
}

// ------------------------------------------------------------------- tasks

/// Member sees only own tasks; patching another's is 403; If-Match guarded.
#[tokio::test]
async fn tasks_member_scoping_and_versioning() {
    let Some((db, world)) = setup().await else {
        return;
    };
    let app = app_for(&db, &world).await;
    let csrf_a = login(&app, world.member_a.email.as_deref().unwrap()).await;

    // Member assigning someone else → 403.
    let res = post(
        &app,
        "/api/v1/tasks",
        json!({"title": "reassign", "owner_principal_id": world.member_b.principal_id}),
        &csrf_a,
        None,
    )
    .await;
    TestApp::problem(res, StatusCode::FORBIDDEN).await;

    let res = post(
        &app,
        "/api/v1/tasks",
        json!({"title": "member a task"}),
        &csrf_a,
        None,
    )
    .await;
    let task = TestApp::data(res).await;
    let tid = task["id"].as_str().unwrap().to_string();
    assert_eq!(
        task["owner_principal_id"],
        json!(world.member_a.principal_id)
    );

    // Member B's list does not contain it (silent scoping).
    let _csrf_b = login(&app, world.member_b.email.as_deref().unwrap()).await;
    let res = app.get("/api/v1/tasks").await;
    let data = TestApp::data(res).await;
    assert!(data.as_array().unwrap().iter().all(|t| t["id"] != tid));
    // Patch someone else's task → 403.
    let res = patch(
        &app,
        &format!("/api/v1/tasks/{tid}"),
        json!({"status": "done"}),
        &_csrf_b,
        Some(1),
    )
    .await;
    TestApp::problem(res, StatusCode::FORBIDDEN).await;

    // Owner: 428 → 412 → 200 done.
    let csrf_a = login(&app, world.member_a.email.as_deref().unwrap()).await;
    let res = patch(
        &app,
        &format!("/api/v1/tasks/{tid}"),
        json!({"status": "done"}),
        &csrf_a,
        None,
    )
    .await;
    TestApp::problem(res, StatusCode::PRECONDITION_REQUIRED).await;
    let res = patch(
        &app,
        &format!("/api/v1/tasks/{tid}"),
        json!({"status": "done"}),
        &csrf_a,
        Some(5),
    )
    .await;
    TestApp::problem(res, StatusCode::PRECONDITION_FAILED).await;
    let res = patch(
        &app,
        &format!("/api/v1/tasks/{tid}"),
        json!({"status": "done"}),
        &csrf_a,
        Some(1),
    )
    .await;
    let done = TestApp::data(res).await;
    assert_eq!(done["status"], "done");

    // Manager sees everyone's tasks.
    let _csrf_mgr = login(&app, world.manager.email.as_deref().unwrap()).await;
    let res = app.get("/api/v1/tasks").await;
    let data = TestApp::data(res).await;
    assert!(data.as_array().unwrap().iter().any(|t| t["id"] == tid));
}

// -------------------------------------------------------------- pagination

/// Cursor pagination: has_more/next_cursor, second page, cursor bound to
/// the filter it was minted for (B.1.5).
#[tokio::test]
async fn companies_list_cursor_pagination() {
    let Some((db, world)) = setup().await else {
        return;
    };
    let app = app_for(&db, &world).await;
    let csrf = login(&app, world.admin.email.as_deref().unwrap()).await;
    for name in ["Page One", "Page Two", "Page Three"] {
        create_company(&app, &csrf, name, None).await;
    }
    let res = app.get("/api/v1/companies?limit=2").await;
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["data"].as_array().unwrap().len(), 2);
    assert_eq!(body["page"]["has_more"], true);
    let cursor = body["page"]["next_cursor"].as_str().unwrap().to_string();

    let res = app
        .get(&format!("/api/v1/companies?limit=2&cursor={cursor}"))
        .await;
    let body: Value = res.json().await.unwrap();
    let page2 = body["data"].as_array().unwrap();
    assert_eq!(page2.len(), 1);
    assert_eq!(body["page"]["has_more"], false);

    // Cursor minted for one filter set is rejected for another.
    let res = app
        .get(&format!(
            "/api/v1/companies?limit=2&cursor={cursor}&q=different"
        ))
        .await;
    TestApp::problem(res, StatusCode::BAD_REQUEST).await;
    // Garbage cursor → 400.
    let res = app.get("/api/v1/companies?cursor=garbage").await;
    TestApp::problem(res, StatusCode::BAD_REQUEST).await;
}
