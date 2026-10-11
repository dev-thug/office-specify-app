//! campaigns module HTTP + job-pipeline tests (spec §7.5, B.3.6–B.3.8,
//! A.6.2). The freeze/activate/schedule handlers run inside the worker in
//! production; tests drive `application::campaigns::*` inside a workspace
//! transaction — the handler bodies are thin wrappers around those calls
//! (registered in `worker::handlers::campaigns::register`).

use api::AppState;
use api::testing::TestApp;
use infrastructure::config::Config;
use infrastructure::db::{Db, ROLE_API};
use infrastructure::testing::{TEST_PASSWORD, TestWorld, seed_workspace};
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;
use uuid::Uuid;

// ---------------------------------------------------------------- harness

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
    resume_workspace(&db, &world).await;
    let db = db.with_default_workspace(Some(world.workspace_id));
    let app = TestApp::spawn_with_state(AppState::with_fake_providers(
        db,
        Arc::new(Config::test()),
        None,
    ))
    .await;
    Some((app, world))
}

/// `workspaces.emergency_stop` defaults to true (fail-closed) — the tests
/// here exercise the steady-state path, so resume the workspace first.
async fn resume_workspace(db: &Db, world: &TestWorld) {
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    sqlx::query("UPDATE workspaces SET emergency_stop = false WHERE id = $1")
        .bind(world.workspace_id)
        .execute(tx.conn())
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

/// A second cookie-jar session (each TestApp::client holds one session).
async fn login(app: &TestApp, email: &str) -> (reqwest::Client, String) {
    let client = reqwest::Client::builder()
        .cookie_store(true)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let res = client
        .post(app.url("/api/v1/auth/login"))
        .json(&json!({"email": email, "password": TEST_PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "login failed for {email}");
    let csrf = TestApp::csrf_token_from(&res).expect("csrf cookie missing");
    (client, csrf)
}

async fn post(
    app: &TestApp,
    client: &reqwest::Client,
    csrf: &str,
    path: &str,
    body: Value,
    idem: Option<&str>,
    if_match: Option<i64>,
) -> reqwest::Response {
    let mut r = client
        .post(app.url(path))
        .header("x-csrf-token", csrf)
        .json(&body);
    if let Some(k) = idem {
        r = r.header("idempotency-key", k);
    }
    if let Some(v) = if_match {
        r = r.header("if-match", format!("\"{v}\""));
    }
    r.send().await.unwrap()
}

async fn get(app: &TestApp, client: &reqwest::Client, path: &str) -> reqwest::Response {
    client.get(app.url(path)).send().await.unwrap()
}

// --------------------------------------------------------------- fixtures

/// Enabled + policy-allowed marketing channel (delivery_provider needs its
/// own integration_connection for the composite provider FK).
async fn marketing_channel(db: &Db, world: &TestWorld) -> (Uuid, Uuid) {
    let ws = world.workspace_id;
    let mut tx = db.begin(ws).await.unwrap();
    let conn: Uuid = sqlx::query_scalar(
        "INSERT INTO integration_connections
            (workspace_id, provider, region, secret_ref, status, connected_by)
         VALUES ($1, 'delivery_provider', 'us', 'dp-secret', 'active', $2)
         RETURNING id",
    )
    .bind(ws)
    .bind(world.admin.principal_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let ident: Uuid = sqlx::query_scalar(
        "INSERT INTO sender_identities
            (workspace_id, mail_account_id, address, display_name, status, verified_at)
         VALUES ($1, $2, 'mk@sender.dev', 'Mk', 'verified', now()) RETURNING id",
    )
    .bind(ws)
    .bind(world.mail_account_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let channel: Uuid = sqlx::query_scalar(
        "INSERT INTO delivery_channels
            (workspace_id, mail_account_id, sender_identity_id, connection_id,
             provider, purpose, name, policy_status, enabled)
         VALUES ($1, $2, $3, $4, 'delivery_provider', 'marketing', 'Bulk',
                 'allowed', true)
         RETURNING id",
    )
    .bind(ws)
    .bind(world.mail_account_id)
    .bind(ident)
    .bind(conn)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    (ident, channel)
}

async fn active_meeting_type(db: &Db, world: &TestWorld, public_base_url: &str) -> Uuid {
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    sqlx::query(
        "UPDATE workspaces SET settings = settings || jsonb_build_object('public_base_url', $2::text)
          WHERE id = $1",
    )
    .bind(world.workspace_id)
    .bind(public_base_url)
    .execute(tx.conn())
    .await
    .unwrap();
    let integration_id: Uuid = sqlx::query_scalar(
        "INSERT INTO integration_connections
            (workspace_id, provider, region, secret_ref, status, connected_by)
         VALUES ($1, 'zoho_calendar', 'us', 'booking-test-ref', 'active', $2)
         RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(world.admin.principal_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let calendar_id: Uuid = sqlx::query_scalar(
        "INSERT INTO calendar_connections
            (workspace_id, host_principal_id, connection_id, provider_calendar_id, status)
         VALUES ($1, $2, $3, $4, 'active') RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(world.admin.principal_id)
    .bind(integration_id)
    .bind(format!("calendar-{}", Uuid::new_v4()))
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let meeting_type_id: Uuid = sqlx::query_scalar(
        "INSERT INTO meeting_types
            (workspace_id, host_principal_id, calendar_connection_id, name,
             duration_minutes, buffer_before_minutes, buffer_after_minutes,
             minimum_notice_minutes, booking_horizon_days)
         VALUES ($1, $2, $3, 'Campaign meeting', 30, 0, 0, 0, 30)
         RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(world.admin.principal_id)
    .bind(calendar_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    meeting_type_id
}

async fn template_version(
    db: &Db,
    world: &TestWorld,
    subject: &str,
    body: &str,
    schema: Value,
) -> Uuid {
    template_version_with_html(db, world, subject, body, None, schema).await
}

async fn template_version_with_html(
    db: &Db,
    world: &TestWorld,
    subject: &str,
    body: &str,
    body_html: Option<&str>,
    schema: Value,
) -> Uuid {
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let template: Uuid = sqlx::query_scalar(
        "INSERT INTO email_templates (workspace_id, name, created_by)
         VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(format!("tpl-{}", Uuid::new_v4()))
    .bind(world.admin.principal_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let tv: Uuid = sqlx::query_scalar(
        "INSERT INTO template_versions
            (workspace_id, template_id, version_no, subject, body_text, body_html, variable_schema,
             content_hash, created_by)
         VALUES ($1, $2, 1, $3, $4, $5, $6, decode(md5(random()::text), 'hex'), $7)
         RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(template)
    .bind(subject)
    .bind(body)
    .bind(body_html)
    .bind(schema)
    .bind(world.admin.principal_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    tv
}

/// email_address + contact + link + (optional) valid marketing basis.
async fn recipient(db: &Db, world: &TestWorld, local: &str, with_basis: bool) -> Uuid {
    let ws = world.workspace_id;
    let mut tx = db.begin(ws).await.unwrap();
    let addr: Uuid = sqlx::query_scalar(
        "INSERT INTO email_addresses
            (workspace_id, address_original, normalized_key, kind, verification_status, verified_at)
         VALUES ($1, $2, $2, 'person', 'verified', now()) RETURNING id",
    )
    .bind(ws)
    .bind(format!("{local}@rcpt.dev"))
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let contact: Uuid = sqlx::query_scalar(
        "INSERT INTO contacts (workspace_id, name) VALUES ($1, $2) RETURNING id",
    )
    .bind(ws)
    .bind(format!("Contact {local}"))
    .fetch_one(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO contact_emails (workspace_id, contact_id, email_address_id, is_primary)
         VALUES ($1, $2, $3, true)",
    )
    .bind(ws)
    .bind(contact)
    .bind(addr)
    .execute(tx.conn())
    .await
    .unwrap();
    if with_basis {
        sqlx::query(
            "INSERT INTO communication_bases
                (workspace_id, email_address_id, purpose, jurisdiction, basis_type,
                 status, evidence_ref, collected_at, reviewed_by, reviewed_at)
             VALUES ($1, $2, 'marketing', 'KR', 'explicit_consent', 'valid',
                     'consent-form-1', now(), $3, now())",
        )
        .bind(ws)
        .bind(addr)
        .bind(world.admin.principal_id)
        .execute(tx.conn())
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
    addr
}

/// Second contact linked to the same address (T31 setup).
async fn second_contact(db: &Db, world: &TestWorld, addr: Uuid) -> Uuid {
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let contact: Uuid = sqlx::query_scalar(
        "INSERT INTO contacts (workspace_id, name) VALUES ($1, 'Second') RETURNING id",
    )
    .bind(world.workspace_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO contact_emails (workspace_id, contact_id, email_address_id)
         VALUES ($1, $2, $3)",
    )
    .bind(world.workspace_id)
    .bind(contact)
    .bind(addr)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    contact
}

/// Simulate the worker claiming `campaigns.freeze_launch` — same
/// application call as `handlers::campaigns` makes.
async fn run_freeze(db: &Db, world: &TestWorld, job_id: Uuid) {
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let payload: Value =
        sqlx::query_scalar("SELECT payload FROM jobs WHERE workspace_id = $1 AND id = $2")
            .bind(world.workspace_id)
            .bind(job_id)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    let launch_id: Uuid = payload["campaign_launch_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let ids: Vec<Uuid> = payload["email_address_ids"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .map(|v| v.as_str().unwrap().parse().unwrap())
        .collect();
    let vars: BTreeMap<String, String> = payload
        .get("variables")
        .cloned()
        .map(|v| serde_json::from_value(v).unwrap())
        .unwrap_or_default();
    let requester_context = payload["requester_context"].clone();
    let tracking_requested = payload
        .get("tracking_requested")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    application::campaigns::freeze_launch(
        &mut tx,
        world.system.principal_id,
        job_id,
        launch_id,
        &ids,
        &vars,
        tracking_requested,
        &requester_context,
    )
    .await
    .unwrap();
    sqlx::query("UPDATE jobs SET state = 'completed', finished_at = now() WHERE workspace_id = $1 AND id = $2")
        .bind(world.workspace_id)
        .bind(job_id)
        .execute(tx.conn())
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

/// Simulate the worker claiming `campaigns.activate_launch`.
async fn run_activate(db: &Db, world: &TestWorld, job_id: Uuid) {
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let payload: Value =
        sqlx::query_scalar("SELECT payload FROM jobs WHERE workspace_id = $1 AND id = $2")
            .bind(world.workspace_id)
            .bind(job_id)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    let launch_id: Uuid = payload["campaign_launch_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let request_id: Uuid = payload["approval_request_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    application::campaigns::activate_launch(
        &mut tx,
        world.system.principal_id,
        job_id,
        launch_id,
        request_id,
    )
    .await
    .unwrap();
    sqlx::query("UPDATE jobs SET state = 'completed', finished_at = now() WHERE workspace_id = $1 AND id = $2")
        .bind(world.workspace_id)
        .bind(job_id)
        .execute(tx.conn())
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

async fn sql_scalar<
    V: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Send + Unpin,
>(
    db: &Db,
    ws: Uuid,
    sql: &str,
    id: Uuid,
) -> V {
    let mut tx = db.begin(ws).await.unwrap();
    let v: V = sqlx::query_scalar(sql)
        .bind(ws)
        .bind(id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    v
}

fn campaign_body(mail_account: Uuid, channel: Uuid, tv: Uuid) -> Value {
    json!({
        "name": "Q4 push",
        "objective": "upsell",
        "mail_account_id": mail_account,
        "delivery_channel_id": channel,
        "timezone": "UTC",
        "schedule_rules": {
            "weekdays": [1, 2, 3, 4, 5, 6, 7],
            "window_start": "00:00",
            "window_end": "23:59"
        },
        "steps": [{"step_order": 1, "template_version_id": tv, "delay_hours": 0}]
    })
}

/// POST /campaigns → (campaign_id, revision_id).
async fn create_campaign(
    app: &TestApp,
    client: &reqwest::Client,
    csrf: &str,
    world: &TestWorld,
    channel: Uuid,
    tv: Uuid,
    idem: &str,
) -> (Uuid, Uuid) {
    let res = post(
        app,
        client,
        csrf,
        "/api/v1/campaigns",
        campaign_body(world.mail_account_id, channel, tv),
        Some(idem),
        None,
    )
    .await;
    let data = TestApp::data(res).await;
    (
        data["campaign_id"].as_str().unwrap().parse().unwrap(),
        data["revision_id"].as_str().unwrap().parse().unwrap(),
    )
}

/// POST /campaigns/{id}/launches → (launch_id, freeze job_id).
async fn create_launch(
    app: &TestApp,
    client: &reqwest::Client,
    csrf: &str,
    campaign: Uuid,
    revision: Uuid,
    recipients: &[Uuid],
    idem: &str,
) -> (Uuid, Uuid) {
    let res = post(
        app,
        client,
        csrf,
        &format!("/api/v1/campaigns/{campaign}/launches"),
        json!({"revision_id": revision, "email_address_ids": recipients}),
        Some(idem),
        None,
    )
    .await;
    assert_eq!(res.status(), StatusCode::ACCEPTED, "launch create failed");
    assert!(
        res.headers().get("location").is_some(),
        "202 must carry a Location to poll (§12.2)"
    );
    let data = TestApp::data(res).await;
    assert_eq!(data["status"], "building");
    (
        data["launch_id"].as_str().unwrap().parse().unwrap(),
        data["job_id"].as_str().unwrap().parse().unwrap(),
    )
}

/// Freeze a launch and return its manifest hash hex.
async fn freeze(db: &Db, job_id: Uuid, world: &TestWorld, launch_id: Uuid) -> String {
    run_freeze(db, world, job_id).await;
    let row: (Option<Vec<u8>>, String) = {
        let mut tx = db.begin(world.workspace_id).await.unwrap();
        let r = sqlx::query_as(
            "SELECT manifest_hash, status FROM campaign_launches WHERE workspace_id=$1 AND id=$2",
        )
        .bind(world.workspace_id)
        .bind(launch_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        tx.rollback().await.unwrap();
        r
    };
    assert_eq!(
        row.1, "awaiting_approval",
        "launch must freeze into awaiting_approval"
    );
    row.0.unwrap().iter().map(|b| format!("{b:02x}")).collect()
}

/// Explicitly queue activation of an approved manifest through the human REST path.
async fn execute_campaign_launch(
    app: &TestApp,
    client: &reqwest::Client,
    csrf: &str,
    launch: Uuid,
    manifest_hash: &str,
    expected_version: i64,
    idem: &str,
) -> Uuid {
    let res = post(
        app,
        client,
        csrf,
        &format!("/api/v1/campaign-launches/{launch}/execute"),
        json!({
            "expected_manifest_hash": manifest_hash,
            "expected_version": expected_version,
        }),
        Some(idem),
        None,
    )
    .await;
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    let data = TestApp::data(res).await;
    assert_eq!(data["launch_status"], "approved");
    assert_eq!(data["execution_status"], "queued");
    data["job_id"].as_str().unwrap().parse().unwrap()
}

// ------------------------------------------------------------------ tests

#[tokio::test]
async fn campaign_create_preflight_launch_freeze_approve_activate() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (admin, acsrf) = login(&app, world.admin.email.as_deref().unwrap()).await;
    let (manager, mcsrf) = login(&app, world.manager.email.as_deref().unwrap()).await;
    let db = &app.state.db;

    let (_ident, channel) = marketing_channel(db, &world).await;
    let tv = template_version(
        db,
        &world,
        "Hi {{contact.name}}",
        "body {{recipient.email}}",
        json!({
            "type": "object",
            "properties": {
                "contact.name": {"type": "string", "maxLength": 300},
                "recipient.email": {"type": "string", "maxLength": 320}
            },
            "required": [],
            "additionalProperties": false
        }),
    )
    .await;
    let addr_a = recipient(db, &world, "alice", true).await;
    let addr_b = recipient(db, &world, "bob", true).await;

    // 1. create — draft + revision 1 + step.
    let res = post(
        &app,
        &admin,
        &acsrf,
        "/api/v1/campaigns",
        campaign_body(world.mail_account_id, channel, tv),
        Some("camp-1"),
        None,
    )
    .await;
    assert_eq!(res.status(), StatusCode::CREATED);
    assert_eq!(
        res.headers().get("etag").unwrap().to_str().unwrap(),
        "\"1\""
    );
    let data = TestApp::data(res).await;
    assert_eq!(data["status"], "draft");
    assert_eq!(data["channel_ready"], true);
    assert_eq!(data["blocking_reasons"], json!([]));
    let campaign: Uuid = data["campaign_id"].as_str().unwrap().parse().unwrap();
    let revision: Uuid = data["revision_id"].as_str().unwrap().parse().unwrap();

    // 2. preflight — informational only.
    let res = post(
        &app,
        &admin,
        &acsrf,
        &format!("/api/v1/campaigns/{campaign}/preflight"),
        json!({"revision_id": revision, "email_address_ids": [addr_a, addr_b]}),
        None,
        None,
    )
    .await;
    let pf = TestApp::data(res).await;
    assert_eq!(pf["is_execution_authorization"], false);
    assert_eq!(pf["global_blocking_reasons"], json!([]));
    assert_eq!(
        pf["recipients"].as_array().unwrap().len(),
        2,
        "both recipients evaluated"
    );
    assert!(
        pf["recipients"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["recipient_checks_passed"] == true)
    );

    // 3. launch → 202 building; freeze job drives it to awaiting_approval.
    let (launch, job) = create_launch(
        &app,
        &admin,
        &acsrf,
        campaign,
        revision,
        &[addr_a, addr_b],
        "launch-1",
    )
    .await;
    let res = get(&app, &admin, &format!("/api/v1/campaign-launches/{launch}")).await;
    let detail = TestApp::data(res).await;
    assert_eq!(detail["status"], "building");
    let manifest_hex = freeze(db, job, &world, launch).await;

    // 4. freeze created the pending approval request pinning the manifest.
    let request_id: Uuid = sql_scalar(
        db,
        world.workspace_id,
        "SELECT id FROM approval_requests WHERE workspace_id=$1 AND launch_id=$2",
        launch,
    )
    .await;

    // Approval detail carries the exact frozen body and actionable policy
    // hints, but only for an approver with mailbox read access.
    let detail =
        TestApp::data(get(&app, &manager, &format!("/api/v1/approvals/{request_id}")).await).await;
    assert_eq!(detail["self_request"], false);
    assert_eq!(detail["can_decide"], true);
    assert_eq!(detail["manifest_hash"], manifest_hex);
    assert_eq!(detail["recipients_count"], 2);
    assert_eq!(detail["snapshot"]["messages"].as_array().unwrap().len(), 2);
    assert!(
        detail["snapshot"]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["body_text"]
                .as_str()
                .is_some_and(|body| body.starts_with("body ") && body.contains("@rcpt.dev")))
    );
    assert!(
        detail["policy_results"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
    );

    // The manager can still approve a request from another principal.
    // 6. manager approves — launch becomes approved, without queuing activation.
    let res = post(
        &app,
        &manager,
        &mcsrf,
        &format!("/api/v1/approvals/{request_id}/decisions"),
        json!({"decision": "approved", "expected_snapshot_hash": manifest_hex}),
        Some("dec-1"),
        Some(1),
    )
    .await;
    let data = TestApp::data(res).await;
    assert_eq!(data["approval_status"], "approved");
    assert_eq!(data["execution_status"], "ready");
    assert!(data["job_id"].is_null());
    let activation_jobs_before: i64 = sql_scalar(
        db,
        world.workspace_id,
        "SELECT count(*) FROM jobs WHERE workspace_id=$1 AND kind='campaigns.activate_launch' AND payload->>'campaign_launch_id'=$2::uuid::text",
        launch,
    )
    .await;
    assert_eq!(
        activation_jobs_before, 0,
        "approval must not activate a launch"
    );
    let marked_before_execute: i64 = sql_scalar(
        db,
        world.workspace_id,
        "SELECT count(*) FROM outbound_messages WHERE workspace_id=$1 AND launch_id=$2 AND explicit_submission_at IS NOT NULL",
        launch,
    )
    .await;
    assert_eq!(marked_before_execute, 0);

    // 7. A human explicitly executes the approved manifest. Repeating the
    // same request or using another key returns the one activation job.
    let approved =
        TestApp::data(get(&app, &admin, &format!("/api/v1/campaign-launches/{launch}")).await)
            .await;
    assert_eq!(approved["status"], "approved");
    assert_eq!(approved["execution_status"], "ready");
    assert!(approved["activation_job_id"].is_null());
    assert!(
        approved["allowed_actions"]
            .as_array()
            .unwrap()
            .contains(&json!("execute_launch"))
    );
    let approved_version = approved["version"].as_i64().unwrap();
    let execute_body = json!({
        "expected_manifest_hash": manifest_hex,
        "expected_version": approved_version,
    });
    // Simulate an approval-only activation job left over from an earlier
    // deployment. It has no executor attribution and cannot activate yet.
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let legacy_activation_job = infrastructure::repo::campaigns::enqueue_dedup(
        &mut tx,
        "campaigns.activate_launch",
        json!({
            "campaign_launch_id": launch,
            "approval_request_id": request_id,
        }),
        chrono::Utc::now(),
        &format!("activate_launch:{launch}"),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    sqlx::query(
        "UPDATE jobs
            SET state='running', lease_owner='test-worker', lease_token=gen_random_uuid(),
                lease_expires_at=now() + interval '1 minute'
          WHERE workspace_id=$1 AND id=$2",
    )
    .bind(world.workspace_id)
    .bind(legacy_activation_job)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    // A worker that picks up the old approval-only job before a human
    // executes it must leave the launch untouched. The job is already
    // running so execute must queue a fresh explicitly authorized job.
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    application::campaigns::activate_launch(
        &mut tx,
        world.system.principal_id,
        legacy_activation_job,
        launch,
        request_id,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let before_execute =
        TestApp::data(get(&app, &admin, &format!("/api/v1/campaign-launches/{launch}")).await)
            .await;
    assert_eq!(before_execute["status"], "approved");
    assert_eq!(before_execute["execution_status"], "ready");
    assert_eq!(before_execute["enrollments"]["pending"], 2);
    assert_eq!(before_execute["enrollments"]["active"], 0);

    let execute_path = format!("/api/v1/campaign-launches/{launch}/execute");

    let bad_manifest = post(
        &app,
        &admin,
        &acsrf,
        &execute_path,
        json!({
            "expected_manifest_hash": "00".repeat(32),
            "expected_version": approved_version,
        }),
        Some("execute-bad-manifest"),
        None,
    )
    .await;
    let p = TestApp::problem(bad_manifest, StatusCode::CONFLICT).await;
    assert_eq!(p["code"], "SNAPSHOT_MISMATCH");

    let stale_version = post(
        &app,
        &admin,
        &acsrf,
        &execute_path,
        json!({
            "expected_manifest_hash": manifest_hex,
            "expected_version": approved_version - 1,
        }),
        Some("execute-stale-version"),
        None,
    )
    .await;
    let p = TestApp::problem(stale_version, StatusCode::PRECONDITION_FAILED).await;
    assert_eq!(p["code"], "VERSION_MISMATCH");

    let res = post(
        &app,
        &admin,
        &acsrf,
        &execute_path,
        execute_body.clone(),
        Some("execute-1"),
        None,
    )
    .await;
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    assert!(res.headers().get("location").is_some());
    assert_eq!(
        res.headers().get("etag").unwrap().to_str().unwrap(),
        format!("\"{}\"", approved_version + 1)
    );
    let execution = TestApp::data(res).await;
    assert_eq!(execution["launch_status"], "approved");
    assert_eq!(execution["execution_status"], "queued");
    assert_eq!(execution["manifest_hash"], manifest_hex);
    assert_eq!(execution["version"], approved_version + 1);
    let activate_job: Uuid = execution["job_id"].as_str().unwrap().parse().unwrap();
    assert_ne!(
        activate_job, legacy_activation_job,
        "a claimed legacy job must not be retagged after it may have already no-op'd"
    );
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    sqlx::query(
        "UPDATE jobs SET state='completed', finished_at=now(), lease_owner=NULL, lease_token=NULL, lease_expires_at=NULL WHERE workspace_id=$1 AND id=$2",
    )
    .bind(world.workspace_id)
    .bind(legacy_activation_job)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let repeated = post(
        &app,
        &admin,
        &acsrf,
        &execute_path,
        execute_body.clone(),
        Some("execute-1"),
        None,
    )
    .await;
    assert_eq!(repeated.status(), StatusCode::ACCEPTED);
    assert_eq!(
        TestApp::data(repeated).await["job_id"],
        activate_job.to_string()
    );

    let queued =
        TestApp::data(get(&app, &admin, &format!("/api/v1/campaign-launches/{launch}")).await)
            .await;
    assert_eq!(queued["status"], "approved");
    assert_eq!(queued["execution_status"], "queued");
    assert_eq!(queued["activation_job_id"], activate_job.to_string());
    let queued_version = queued["version"].as_i64().unwrap();
    assert_eq!(queued_version, approved_version + 1);
    let second_key = post(
        &app,
        &admin,
        &acsrf,
        &execute_path,
        json!({
            "expected_manifest_hash": manifest_hex,
            "expected_version": queued_version,
        }),
        Some("execute-2"),
        None,
    )
    .await;
    assert_eq!(second_key.status(), StatusCode::ACCEPTED);
    let second_execution = TestApp::data(second_key).await;
    assert_eq!(second_execution["job_id"], activate_job.to_string());
    assert_eq!(second_execution["version"], queued_version);

    let activation_jobs_after: i64 = sql_scalar(
        db,
        world.workspace_id,
        "SELECT count(*) FROM jobs WHERE workspace_id=$1 AND kind='campaigns.activate_launch' AND dedupe_key='explicit_activate_launch:' || $2::uuid::text",
        launch,
    )
    .await;
    assert_eq!(
        activation_jobs_after, 1,
        "execution must queue activation once"
    );

    // 8. activation — launch runs, enrollments active, step-1 outbound queued.
    run_activate(db, &world, activate_job).await;
    let res = get(&app, &admin, &format!("/api/v1/campaign-launches/{launch}")).await;
    let detail = TestApp::data(res).await;
    assert_eq!(detail["status"], "running");
    assert_eq!(detail["execution_status"], "activated");
    assert_eq!(detail["activation_job_id"], activate_job.to_string());
    assert_eq!(detail["enrollments"]["active"], 2);

    let repeated_after_activation = post(
        &app,
        &admin,
        &acsrf,
        &execute_path,
        json!({
            "expected_manifest_hash": manifest_hex,
            "expected_version": approved_version,
        }),
        Some("execute-after-activation"),
        None,
    )
    .await;
    let p = TestApp::problem(repeated_after_activation, StatusCode::PRECONDITION_FAILED).await;
    assert_eq!(p["code"], "VERSION_MISMATCH");
    assert_eq!(detail["enrollments"]["pending"], 0);
    let marked_after_activation: i64 = sql_scalar(
        db,
        world.workspace_id,
        "SELECT count(*) FROM outbound_messages WHERE workspace_id=$1 AND launch_id=$2 AND explicit_submission_at IS NOT NULL",
        launch,
    )
    .await;
    assert_eq!(marked_after_activation, 2);
    let matched_dispatch_markers: i64 = sql_scalar(
        db,
        world.workspace_id,
        "SELECT count(*) FROM jobs j
           JOIN outbound_messages o
             ON o.workspace_id=j.workspace_id
            AND j.payload->>'outbound_message_id'=o.id::text
          WHERE j.workspace_id=$1 AND o.launch_id=$2
            AND j.kind='sending.dispatch_outbound'
            AND NULLIF(j.payload->>'explicit_submission_at','')::timestamptz
                = o.explicit_submission_at",
        launch,
    )
    .await;
    assert_eq!(matched_dispatch_markers, 2);
    let msgs = detail["messages"]["items"].as_array().unwrap();
    assert_eq!(msgs.len(), 2, "one frozen outbound per enrolled recipient");
    assert!(msgs.iter().all(|m| m["status"] == "queued"));

    // dispatch jobs queued for each outbound.
    let dispatch_jobs: i64 = sql_scalar(
        db,
        world.workspace_id,
        "SELECT count(*) FROM jobs WHERE workspace_id=$1 AND workspace_id=$2 AND kind='sending.dispatch_outbound' AND state='queued'",
        world.workspace_id,
    )
    .await;
    assert_eq!(dispatch_jobs, 2);
}

#[tokio::test]
async fn t31_one_address_two_contacts_single_enrollment() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (admin, csrf) = login(&app, world.admin.email.as_deref().unwrap()).await;
    let db = &app.state.db;

    let (_i, channel) = marketing_channel(db, &world).await;
    let tv = template_version(db, &world, "s", "b", json!({})).await;
    let addr = recipient(db, &world, "shared", true).await;
    second_contact(db, &world, addr).await;

    let (campaign, revision) =
        create_campaign(&app, &admin, &csrf, &world, channel, tv, "t31-camp").await;
    let (launch, job) = create_launch(
        &app,
        &admin,
        &csrf,
        campaign,
        revision,
        &[addr],
        "t31-launch",
    )
    .await;
    run_freeze(db, &world, job).await;

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let (n_enroll, n_outbound): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM campaign_enrollments
                   WHERE workspace_id=$1 AND launch_id=$2 AND email_address_id=$3),
                (SELECT count(*) FROM outbound_messages
                   WHERE workspace_id=$1 AND launch_id=$2 AND to_email_address_id=$3)",
    )
    .bind(world.workspace_id)
    .bind(launch)
    .bind(addr)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(
        n_enroll, 1,
        "T31: exactly one enrollment per address per launch"
    );
    assert_eq!(
        n_outbound, 1,
        "one frozen outbound for the single enrollment's step"
    );
}

#[tokio::test]
async fn campaign_launch_freeze_request_carries_tracking_opt_in() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (admin, csrf) = login(&app, world.admin.email.as_deref().unwrap()).await;
    let db = &app.state.db;
    let (_identity, channel) = marketing_channel(db, &world).await;
    let template = template_version_with_html(
        db,
        &world,
        "Offer",
        "Read more at https://example.test/offer",
        Some("<p><a href=\"https://example.test/offer\">Read more</a></p>"),
        json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
    )
    .await;
    let recipient = recipient(db, &world, "tracking-opt-in", true).await;
    let (campaign_id, revision_id) = create_campaign(
        &app,
        &admin,
        &csrf,
        &world,
        channel,
        template,
        "tracking-opt-in-campaign",
    )
    .await;

    let response = post(
        &app,
        &admin,
        &csrf,
        &format!("/api/v1/campaigns/{campaign_id}/launches"),
        json!({
            "revision_id": revision_id,
            "email_address_ids": [recipient],
            "tracking_requested": true,
        }),
        Some("tracking-opt-in-launch"),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let data = TestApp::data(response).await;
    let job_id: Uuid = data["job_id"].as_str().unwrap().parse().unwrap();
    let launch_id: Uuid = data["launch_id"].as_str().unwrap().parse().unwrap();
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let payload: Value =
        sqlx::query_scalar("SELECT payload FROM jobs WHERE workspace_id=$1 AND id=$2")
            .bind(world.workspace_id)
            .bind(job_id)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(payload["tracking_requested"], true);

    run_freeze(db, &world, job_id).await;
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    #[allow(clippy::type_complexity)]
    let outbound: (
        Uuid,
        Option<Uuid>,
        Option<String>,
        String,
        String,
        Option<String>,
        Option<String>,
        Vec<u8>,
    ) = sqlx::query_as(
        "SELECT id, thread_id, internet_message_id, from_address_snapshot,
                    to_address_snapshot, body_text, body_html, payload_hash
               FROM outbound_messages WHERE workspace_id=$1 AND launch_id=$2",
    )
    .bind(world.workspace_id)
    .bind(launch_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let tracking_capabilities: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM outbound_engagement_capabilities
          WHERE workspace_id=$1 AND outbound_message_id=$2",
    )
    .bind(world.workspace_id)
    .bind(outbound.0)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.rollback().await.unwrap();

    let thread_id = outbound.1.expect("campaign outbound has a local thread");
    let message_id = outbound.2.expect("campaign outbound has stable RFC ID");
    let body_text = outbound.5.expect("campaign outbound stores body text");
    assert!(body_text.contains("/public/v1/engagement/click/"));
    assert!(body_text.contains("/public/unsubscribe/"));
    assert_eq!(
        tracking_capabilities, 2,
        "one click and one open capability"
    );
    let expected_payload_hash =
        infrastructure::crypto::sha256_bytes(&domain::campaigns::payload_bytes_with_context(
            &outbound.3,
            &outbound.4,
            "Offer",
            &body_text,
            outbound.6.as_deref(),
            Some(&thread_id.to_string()),
            Some(&message_id),
        ));
    assert_eq!(outbound.7, expected_payload_hash);
}

#[tokio::test]
async fn campaign_freeze_mints_recipient_bound_booking_and_unsubscribe_links() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (admin, csrf) = login(&app, world.admin.email.as_deref().unwrap()).await;
    let db = &app.state.db;
    let (_ident, channel) = marketing_channel(db, &world).await;
    let meeting_type_id = active_meeting_type(db, &world, "https://book.example").await;
    let schema = json!({
        "type": "object",
        "properties": {
            "contact.name": {"type": "string", "maxLength": 300},
            "meeting.booking_url": {"type": "string", "format": "uri"},
            "compliance.unsubscribe_url": {"type": "string", "format": "uri"}
        },
        "required": ["contact.name", "meeting.booking_url", "compliance.unsubscribe_url"],
        "additionalProperties": false
    });
    let tv = template_version_with_html(
        db,
        &world,
        "Hello {{contact.name}}",
        "Hello {{contact.name}} {{meeting.booking_url}} {{compliance.unsubscribe_url}}",
        Some("<p>{{contact.name}}</p><a href=\"{{meeting.booking_url}}\">book</a>"),
        schema,
    )
    .await;
    let addr = recipient(db, &world, "canonical-render", true).await;
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    sqlx::query(
        "UPDATE contacts SET name = $3
          WHERE workspace_id = $1 AND id = (
              SELECT contact_id FROM contact_emails
               WHERE workspace_id = $1 AND email_address_id = $2 LIMIT 1
          )",
    )
    .bind(world.workspace_id)
    .bind(addr)
    .bind("<b>A&B</b>")
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mut campaign_request = campaign_body(world.mail_account_id, channel, tv);
    campaign_request["name"] = json!("bound-booking-url");
    campaign_request["schedule_rules"]["meeting_type_id"] = json!(meeting_type_id);
    let res = post(
        &app,
        &admin,
        &csrf,
        "/api/v1/campaigns",
        campaign_request,
        Some("bound-booking-url-campaign"),
        None,
    )
    .await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let campaign = TestApp::data(res).await;
    let campaign_id: Uuid = campaign["campaign_id"].as_str().unwrap().parse().unwrap();
    let revision_id: Uuid = campaign["revision_id"].as_str().unwrap().parse().unwrap();
    let (launch_id, job) = create_launch(
        &app,
        &admin,
        &csrf,
        campaign_id,
        revision_id,
        &[addr],
        "canonical-render-launch",
    )
    .await;
    freeze(db, job, &world, launch_id).await;

    let detail = TestApp::data(
        get(
            &app,
            &admin,
            &format!("/api/v1/campaign-launches/{launch_id}"),
        )
        .await,
    )
    .await;
    let frozen_message = &detail["messages"]["items"][0];
    let frozen_body = frozen_message["body_text"].as_str().unwrap();
    let body_parts: Vec<_> = frozen_body.split_whitespace().collect();
    assert_eq!(body_parts[0], "Hello");
    assert_eq!(body_parts[1], "<b>A&B</b>");
    assert!(body_parts[2].starts_with("https://book.example/public/book/"));
    assert!(body_parts[3].starts_with("https://book.example/public/unsubscribe/"));
    let unsubscribe_url = body_parts[3];
    assert_eq!(
        frozen_message["body_html"],
        format!(
            "<p>&lt;b&gt;A&amp;B&lt;/b&gt;</p><a href=\"{}\" rel=\"noopener noreferrer\">book</a>\n<p><a href=\"{}\">Unsubscribe</a></p>",
            body_parts[2], unsubscribe_url
        )
    );

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let (outbound_id, subject, body_text, body_html): (Uuid, String, String, Option<String>) =
        sqlx::query_as(
            "SELECT id, subject, body_text, body_html FROM outbound_messages
              WHERE workspace_id = $1 AND launch_id = $2",
        )
        .bind(world.workspace_id)
        .bind(launch_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
    assert_eq!(subject, "Hello <b>A&B</b>");
    let body_parts: Vec<_> = body_text.split_whitespace().collect();
    assert!(body_parts[2].starts_with("https://book.example/public/book/"));
    assert!(body_parts[3].starts_with("https://book.example/public/unsubscribe/"));
    let booking_token = body_parts[2].rsplit('/').next().unwrap();
    let unsubscribe_token = unsubscribe_url.rsplit('/').next().unwrap();
    assert_eq!(
        body_html.as_deref(),
        Some(frozen_message["body_html"].as_str().unwrap())
    );
    let stored_hash: Vec<u8> = sqlx::query_scalar(
        "SELECT token_hash FROM unsubscribe_tokens
          WHERE workspace_id = $1 AND outbound_message_id = $2",
    )
    .bind(world.workspace_id)
    .bind(outbound_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let (booking_hash, bound_email_address_id): (Vec<u8>, Uuid) = sqlx::query_as(
        "SELECT token_hash, bound_email_address_id FROM booking_links
          WHERE workspace_id = $1 AND meeting_type_id = $2",
    )
    .bind(world.workspace_id)
    .bind(meeting_type_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    let expected_hash = infrastructure::crypto::sha256_bytes(unsubscribe_token.as_bytes());
    assert_eq!(stored_hash.as_slice(), &expected_hash[..]);
    let expected_booking_hash = infrastructure::crypto::sha256_bytes(booking_token.as_bytes());
    assert_eq!(booking_hash.as_slice(), &expected_booking_hash[..]);
    assert_eq!(bound_email_address_id, addr);
}

#[tokio::test]
async fn revoked_agent_credential_blocks_booking_link_creation_during_freeze() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let db = &app.state.db;
    let (_ident, channel) = marketing_channel(db, &world).await;
    let meeting_type_id = active_meeting_type(db, &world, "https://book.example").await;
    let template_schema = json!({
        "type": "object",
        "properties": {
            "meeting.booking_url": {"type": "string", "format": "uri"}
        },
        "required": ["meeting.booking_url"],
        "additionalProperties": false
    });
    let template_version = template_version(
        db,
        &world,
        "Meeting invitation",
        "Book here: {{meeting.booking_url}}",
        template_schema,
    )
    .await;
    let email_address_id = recipient(db, &world, "revoked-agent", true).await;
    let agent_scopes = [
        "campaign.compose".to_string(),
        "booking.propose".to_string(),
        "booking.read".to_string(),
    ];
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    sqlx::query(
        "INSERT INTO mail_account_grants
            (workspace_id, mail_account_id, principal_id, can_read, can_compose, granted_by)
         VALUES ($1, $2, $3, true, true, $4)",
    )
    .bind(world.workspace_id)
    .bind(world.mail_account_id)
    .bind(world.agent.principal_id)
    .bind(world.admin.principal_id)
    .execute(tx.conn())
    .await
    .unwrap();
    let (credential_id, _credential_secret) =
        infrastructure::repo::auth::create_service_credential(
            &mut tx,
            world.agent.principal_id,
            "campaign-booking",
            &agent_scopes,
            chrono::Duration::days(1),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let agent_ctx = infrastructure::testing::ctx_for_credential(
        &world,
        &world.agent,
        credential_id,
        contracts::ScopeSet::of([
            contracts::Scope::CampaignCompose,
            contracts::Scope::BookingPropose,
            contracts::Scope::BookingRead,
        ]),
        "revoked-agent-booking-freeze",
    );
    let request = contracts::campaigns::CreateCampaignRequest {
        name: "Agent booking launch".into(),
        objective: Some("Invite one recipient".into()),
        mail_account_id: world.mail_account_id,
        delivery_channel_id: channel,
        timezone: "UTC".into(),
        schedule_rules: contracts::campaigns::ScheduleRulesInput {
            weekdays: vec![1, 2, 3, 4, 5, 6, 7],
            window_start: Some("00:00".into()),
            window_end: Some("23:59".into()),
            meeting_type_id: Some(meeting_type_id),
            ..Default::default()
        },
        steps: vec![contracts::campaigns::CampaignStepInput {
            step_order: 1,
            template_version_id: template_version,
            delay_hours: 0,
        }],
        stop_on_reply: true,
        stop_on_booking: true,
    };
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let campaign = application::campaigns::create_campaign(&mut tx, &agent_ctx, &request)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let launch = application::campaigns::create_launch(
        &mut tx,
        &agent_ctx,
        campaign.campaign_id,
        &contracts::campaigns::CreateLaunchRequest {
            revision_id: campaign.revision_id,
            email_address_ids: vec![email_address_id],
            tracking_requested: false,
            variables: Default::default(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    sqlx::query(
        "UPDATE service_credentials SET revoked_at = now()
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(world.workspace_id)
    .bind(credential_id)
    .execute(tx.conn())
    .await
    .unwrap();
    let payload: Value =
        sqlx::query_scalar("SELECT payload FROM jobs WHERE workspace_id = $1 AND id = $2")
            .bind(world.workspace_id)
            .bind(launch.job_id)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    let err = application::campaigns::freeze_launch(
        &mut tx,
        world.system.principal_id,
        launch.job_id,
        launch.launch_id,
        &[email_address_id],
        &BTreeMap::new(),
        false,
        &payload["requester_context"],
    )
    .await
    .expect_err("revoked service credential cannot mint recipient capabilities");
    assert_eq!(err.code, contracts::ErrorCode::ScopeRequired);
    tx.rollback().await.unwrap();

    let links: i64 = sql_scalar(
        db,
        world.workspace_id,
        "SELECT count(*) FROM booking_links WHERE workspace_id=$1 AND bound_email_address_id=$2",
        email_address_id,
    )
    .await;
    assert_eq!(links, 0);
}

#[tokio::test]
async fn campaign_freeze_rejects_user_supplied_meeting_booking_url() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (admin, csrf) = login(&app, world.admin.email.as_deref().unwrap()).await;
    let db = &app.state.db;
    let (_ident, channel) = marketing_channel(db, &world).await;
    let meeting_type_id = active_meeting_type(db, &world, "https://book.example").await;
    let tv = template_version(
        db,
        &world,
        "Book",
        "{{meeting.booking_url}}",
        json!({
            "type": "object",
            "properties": {
                "meeting.booking_url": {"type": "string", "format": "uri"}
            },
            "required": ["meeting.booking_url"],
            "additionalProperties": false
        }),
    )
    .await;
    let email_address_id = recipient(db, &world, "no-override", true).await;
    let mut campaign_request = campaign_body(world.mail_account_id, channel, tv);
    campaign_request["name"] = json!("no-booking-url-override");
    campaign_request["schedule_rules"]["meeting_type_id"] = json!(meeting_type_id);
    let res = post(
        &app,
        &admin,
        &csrf,
        "/api/v1/campaigns",
        campaign_request,
        Some("no-booking-url-override-campaign"),
        None,
    )
    .await;
    let campaign = TestApp::data(res).await;
    let campaign_id: Uuid = campaign["campaign_id"].as_str().unwrap().parse().unwrap();
    let revision_id: Uuid = campaign["revision_id"].as_str().unwrap().parse().unwrap();

    let ctx = infrastructure::testing::ctx_for(&world, &world.admin, "booking-override-freeze");
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let launch = application::campaigns::create_launch(
        &mut tx,
        &ctx,
        campaign_id,
        &contracts::campaigns::CreateLaunchRequest {
            revision_id,
            email_address_ids: vec![email_address_id],
            tracking_requested: false,
            variables: BTreeMap::from([(
                "meeting.booking_url".into(),
                "https://attacker.example/collect".into(),
            )]),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let payload: Value =
        sqlx::query_scalar("SELECT payload FROM jobs WHERE workspace_id = $1 AND id = $2")
            .bind(world.workspace_id)
            .bind(launch.job_id)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    let error = application::campaigns::freeze_launch(
        &mut tx,
        world.system.principal_id,
        launch.job_id,
        launch.launch_id,
        &[email_address_id],
        &BTreeMap::from([(
            "meeting.booking_url".into(),
            "https://attacker.example/collect".into(),
        )]),
        false,
        &payload["requester_context"],
    )
    .await
    .expect_err("system booking URL cannot be caller-supplied");
    assert_eq!(error.code, contracts::ErrorCode::ValidationFailed);
    tx.rollback().await.unwrap();

    let links: i64 = sql_scalar(
        db,
        world.workspace_id,
        "SELECT count(*) FROM booking_links WHERE workspace_id=$1 AND bound_email_address_id=$2",
        email_address_id,
    )
    .await;
    assert_eq!(
        links, 0,
        "failed freeze must roll back the minted capability"
    );
}

#[tokio::test]
async fn campaign_marketing_freeze_adds_unsubscribe_footer_without_template_variable() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (admin, csrf) = login(&app, world.admin.email.as_deref().unwrap()).await;
    let db = &app.state.db;
    let (_ident, channel) = marketing_channel(db, &world).await;
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    sqlx::query(
        "UPDATE workspaces SET settings = settings || jsonb_build_object('public_base_url', $2::text)
          WHERE id = $1",
    )
    .bind(world.workspace_id)
    .bind("https://public.example")
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let tv = template_version_with_html(
        db,
        &world,
        "Hello",
        "Hello there",
        Some("<p>Hello there</p>"),
        json!({"type": "object", "properties": {}, "additionalProperties": false}),
    )
    .await;
    let addr = recipient(db, &world, "implicit-unsubscribe", true).await;
    let (campaign, revision) = create_campaign(
        &app,
        &admin,
        &csrf,
        &world,
        channel,
        tv,
        "implicit-unsubscribe",
    )
    .await;
    let (launch_id, job) = create_launch(
        &app,
        &admin,
        &csrf,
        campaign,
        revision,
        &[addr],
        "implicit-unsubscribe-launch",
    )
    .await;
    freeze(db, job, &world, launch_id).await;

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let (outbound_id, body_text, body_html): (Uuid, String, String) = sqlx::query_as(
        "SELECT id, body_text, body_html FROM outbound_messages
          WHERE workspace_id = $1 AND launch_id = $2",
    )
    .bind(world.workspace_id)
    .bind(launch_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert!(body_text.contains("Unsubscribe: https://public.example/public/unsubscribe/"));
    assert!(body_html.contains("href=\"https://public.example/public/unsubscribe/"));
    let url = body_text.split_whitespace().last().unwrap();
    let token = url.rsplit('/').next().unwrap();
    let token_hash: Vec<u8> = sqlx::query_scalar(
        "SELECT token_hash FROM unsubscribe_tokens
          WHERE workspace_id = $1 AND outbound_message_id = $2",
    )
    .bind(world.workspace_id)
    .bind(outbound_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(
        token_hash.as_slice(),
        &infrastructure::crypto::sha256_bytes(token.as_bytes())[..]
    );
}

#[tokio::test]
async fn mailbox_acl_denies_non_grantee_and_list_is_filtered() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (admin, acsrf) = login(&app, world.admin.email.as_deref().unwrap()).await;
    let (member_b, bcsrf) = login(&app, world.member_b.email.as_deref().unwrap()).await;
    let db = &app.state.db;

    let (_i, channel) = marketing_channel(db, &world).await;
    let tv = template_version(db, &world, "s", "b", json!({})).await;
    let (campaign, _rev) =
        create_campaign(&app, &admin, &acsrf, &world, channel, tv, "acl-camp").await;

    // member_b has no grant on the mailbox: read + write both denied.
    let res = get(&app, &member_b, &format!("/api/v1/campaigns/{campaign}")).await;
    let p = TestApp::problem(res, StatusCode::FORBIDDEN).await;
    assert_eq!(p["code"], "MAILBOX_ACCESS_DENIED");

    let res = post(
        &app,
        &member_b,
        &bcsrf,
        "/api/v1/campaigns",
        campaign_body(world.mail_account_id, channel, tv),
        Some("b-camp"),
        None,
    )
    .await;
    let p = TestApp::problem(res, StatusCode::FORBIDDEN).await;
    assert_eq!(p["code"], "MAILBOX_ACCESS_DENIED");

    // List filters to granted mailboxes — member_b sees none.
    let res = get(&app, &member_b, "/api/v1/campaigns").await;
    let data = TestApp::data(res).await;
    assert_eq!(data.as_array().unwrap().len(), 0);
}

/// §5.3: a member holding the mailbox can_compose grant can create
/// campaigns — the grant alone authorizes (no admin implicit scope).
#[tokio::test]
async fn member_with_compose_grant_can_create_campaign() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (member_a, csrf) = login(&app, world.member_a.email.as_deref().unwrap()).await;
    let db = &app.state.db;
    let (_i, channel) = marketing_channel(db, &world).await;
    let tv = template_version(db, &world, "s", "b", json!({})).await;

    let res = post(
        &app,
        &member_a,
        &csrf,
        "/api/v1/campaigns",
        campaign_body(world.mail_account_id, channel, tv),
        Some("member-camp"),
        None,
    )
    .await;
    assert_eq!(
        res.status(),
        StatusCode::CREATED,
        "compose grant must suffice"
    );
}

#[tokio::test]
async fn campaign_create_is_idempotent() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (admin, csrf) = login(&app, world.admin.email.as_deref().unwrap()).await;
    let db = &app.state.db;
    let (_i, channel) = marketing_channel(db, &world).await;
    let tv = template_version(db, &world, "s", "b", json!({})).await;
    let body = campaign_body(world.mail_account_id, channel, tv);

    let res = post(
        &app,
        &admin,
        &csrf,
        "/api/v1/campaigns",
        body.clone(),
        Some("idem-1"),
        None,
    )
    .await;
    let first = TestApp::data(res).await;

    // Same key + same body → replays the stored response, no second row.
    let res = post(
        &app,
        &admin,
        &csrf,
        "/api/v1/campaigns",
        body,
        Some("idem-1"),
        None,
    )
    .await;
    let second = TestApp::data(res).await;
    assert_eq!(first["campaign_id"], second["campaign_id"]);

    let count: i64 = sql_scalar(
        db,
        world.workspace_id,
        "SELECT count(*) FROM campaigns WHERE workspace_id=$1 AND id=$2",
        first["campaign_id"].as_str().unwrap().parse().unwrap(),
    )
    .await;
    assert_eq!(count, 1);

    // Same key + different body → 409 IDEMPOTENCY_KEY_REUSED.
    let mut other = campaign_body(world.mail_account_id, channel, tv);
    other["name"] = json!("different name");
    let res = post(
        &app,
        &admin,
        &csrf,
        "/api/v1/campaigns",
        other,
        Some("idem-1"),
        None,
    )
    .await;
    let p = TestApp::problem(res, StatusCode::CONFLICT).await;
    assert_eq!(p["code"], "IDEMPOTENCY_KEY_REUSED");
}

#[tokio::test]
async fn campaign_authoring_rejects_follow_up_steps_and_delays() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (admin, csrf) = login(&app, world.admin.email.as_deref().unwrap()).await;
    let db = &app.state.db;
    let (_identity, channel) = marketing_channel(db, &world).await;
    let tv = template_version(db, &world, "s", "b", json!({})).await;

    let mut delayed = campaign_body(world.mail_account_id, channel, tv);
    delayed["name"] = json!("delayed campaign");
    delayed["steps"] = json!([
        {"step_order": 1, "template_version_id": tv, "delay_hours": 1}
    ]);
    let res = post(
        &app,
        &admin,
        &csrf,
        "/api/v1/campaigns",
        delayed,
        Some("reject-delayed-campaign"),
        None,
    )
    .await;
    let p = TestApp::problem(res, StatusCode::BAD_REQUEST).await;
    assert_eq!(p["code"], "VALIDATION_FAILED");

    let mut follow_up = campaign_body(world.mail_account_id, channel, tv);
    follow_up["name"] = json!("multi-step campaign");
    follow_up["steps"] = json!([
        {"step_order": 1, "template_version_id": tv, "delay_hours": 0},
        {"step_order": 2, "template_version_id": tv, "delay_hours": 0}
    ]);
    let res = post(
        &app,
        &admin,
        &csrf,
        "/api/v1/campaigns",
        follow_up,
        Some("reject-follow-up-campaign"),
        None,
    )
    .await;
    let p = TestApp::problem(res, StatusCode::BAD_REQUEST).await;
    assert_eq!(p["code"], "VALIDATION_FAILED");

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let campaigns: i64 =
        sqlx::query_scalar("SELECT count(*) FROM campaigns WHERE workspace_id = $1")
            .bind(world.workspace_id)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    assert_eq!(
        campaigns, 0,
        "invalid automation must not create a campaign"
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn revision_authoring_rejects_follow_up_steps_and_delays() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (admin, csrf) = login(&app, world.admin.email.as_deref().unwrap()).await;
    let db = &app.state.db;
    let (_identity, channel) = marketing_channel(db, &world).await;
    let tv = template_version(db, &world, "s", "b", json!({})).await;
    let (campaign, _) =
        create_campaign(&app, &admin, &csrf, &world, channel, tv, "rev-single-step").await;

    let mut delayed = json!({
        "delivery_channel_id": channel,
        "timezone": "UTC",
        "schedule_rules": {"weekdays": [1], "window_start": "09:00", "window_end": "18:00"},
        "steps": [{"step_order": 1, "template_version_id": tv, "delay_hours": 1}]
    });
    let res = post(
        &app,
        &admin,
        &csrf,
        &format!("/api/v1/campaigns/{campaign}/revisions"),
        delayed.take(),
        Some("reject-delayed-revision"),
        Some(1),
    )
    .await;
    let p = TestApp::problem(res, StatusCode::BAD_REQUEST).await;
    assert_eq!(p["code"], "VALIDATION_FAILED");

    let follow_up = json!({
        "delivery_channel_id": channel,
        "timezone": "UTC",
        "schedule_rules": {"weekdays": [1], "window_start": "09:00", "window_end": "18:00"},
        "steps": [
            {"step_order": 1, "template_version_id": tv, "delay_hours": 0},
            {"step_order": 2, "template_version_id": tv, "delay_hours": 0}
        ]
    });
    let res = post(
        &app,
        &admin,
        &csrf,
        &format!("/api/v1/campaigns/{campaign}/revisions"),
        follow_up,
        Some("reject-follow-up-revision"),
        Some(1),
    )
    .await;
    let p = TestApp::problem(res, StatusCode::BAD_REQUEST).await;
    assert_eq!(p["code"], "VALIDATION_FAILED");

    let detail =
        TestApp::data(get(&app, &admin, &format!("/api/v1/campaigns/{campaign}")).await).await;
    assert_eq!(detail["revisions"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn new_launch_rejects_preserved_legacy_follow_up_revision() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (admin, csrf) = login(&app, world.admin.email.as_deref().unwrap()).await;
    let db = &app.state.db;
    let (_identity, channel) = marketing_channel(db, &world).await;
    let tv = template_version(db, &world, "s", "b", json!({})).await;
    let (campaign, _) =
        create_campaign(&app, &admin, &csrf, &world, channel, tv, "legacy-revision").await;

    let legacy_revision = {
        let mut tx = db.begin(world.workspace_id).await.unwrap();
        let revision_id: Uuid = sqlx::query_scalar(
            "INSERT INTO campaign_revisions
                (workspace_id, campaign_id, mail_account_id, delivery_channel_id,
                 revision_no, timezone, schedule_rules, created_by)
             VALUES ($1, $2, $3, $4, 2, 'UTC',
                     '{\"weekdays\":[1,2,3,4,5,6,7],\"window_start\":\"00:00\",\"window_end\":\"23:59\"}', $5)
             RETURNING id",
        )
        .bind(world.workspace_id)
        .bind(campaign)
        .bind(world.mail_account_id)
        .bind(channel)
        .bind(world.admin.principal_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        for step_order in [1_i32, 2] {
            sqlx::query(
                "INSERT INTO campaign_steps
                    (workspace_id, revision_id, template_version_id, step_order, delay_hours)
                 VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(world.workspace_id)
            .bind(revision_id)
            .bind(tv)
            .bind(step_order)
            .bind(if step_order == 1 { 0 } else { 24 })
            .execute(tx.conn())
            .await
            .unwrap();
        }
        tx.commit().await.unwrap();
        revision_id
    };

    let recipient = recipient(db, &world, "legacy-follow-up", true).await;
    let res = post(
        &app,
        &admin,
        &csrf,
        &format!("/api/v1/campaigns/{campaign}/launches"),
        json!({"revision_id": legacy_revision, "email_address_ids": [recipient]}),
        Some("reject-legacy-multistep-launch"),
        None,
    )
    .await;
    let p = TestApp::problem(res, StatusCode::CONFLICT).await;
    assert_eq!(p["code"], "CONFLICT");

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let retained_steps: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM campaign_steps WHERE workspace_id=$1 AND revision_id=$2",
    )
    .bind(world.workspace_id)
    .bind(legacy_revision)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let launches: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM campaign_launches WHERE workspace_id=$1 AND revision_id=$2",
    )
    .bind(world.workspace_id)
    .bind(legacy_revision)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(retained_steps, 2, "legacy revision data must be retained");
    assert_eq!(launches, 0, "rejected launch must have no side effects");
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn accepted_legacy_step_cancels_queued_follow_ups_without_deleting_revision() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (admin, csrf) = login(&app, world.admin.email.as_deref().unwrap()).await;
    let db = &app.state.db;
    let (identity, channel) = marketing_channel(db, &world).await;
    let tv = template_version(db, &world, "s", "b", json!({})).await;
    let (campaign, _) = create_campaign(
        &app,
        &admin,
        &csrf,
        &world,
        channel,
        tv,
        "legacy-follow-up-stop",
    )
    .await;
    let address = recipient(db, &world, "legacy-flow", true).await;

    let (legacy_revision, enrollment_id, first_outbound, follow_up_outbound, dispatch_job_id) = {
        let mut tx = db.begin(world.workspace_id).await.unwrap();
        let legacy_revision = infrastructure::repo::campaigns::insert_revision(
            &mut tx,
            campaign,
            world.mail_account_id,
            channel,
            2,
            "UTC",
            &json!({
                "weekdays": [1, 2, 3, 4, 5, 6, 7],
                "window_start": "00:00",
                "window_end": "23:59"
            }),
            true,
            true,
            world.admin.principal_id,
        )
        .await
        .unwrap();
        let first_step =
            infrastructure::repo::campaigns::insert_step(&mut tx, legacy_revision, tv, 1, 0)
                .await
                .unwrap();
        let follow_up_step =
            infrastructure::repo::campaigns::insert_step(&mut tx, legacy_revision, tv, 2, 24)
                .await
                .unwrap();
        let launch = infrastructure::repo::campaigns::insert_launch(
            &mut tx,
            legacy_revision,
            channel,
            world.admin.principal_id,
            chrono::Utc::now() + chrono::Duration::days(2),
        )
        .await
        .unwrap();
        let enrollment = infrastructure::repo::campaigns::insert_enrollment(
            &mut tx,
            launch,
            legacy_revision,
            address,
            None,
            "pending",
            None,
        )
        .await
        .unwrap();
        infrastructure::repo::campaigns::freeze_revision(&mut tx, legacy_revision)
            .await
            .unwrap();
        infrastructure::repo::campaigns::freeze_launch_row(&mut tx, launch, &[7_u8; 32])
            .await
            .unwrap();
        for status in ["awaiting_approval", "approved", "running"] {
            infrastructure::repo::campaigns::transition_launch(&mut tx, launch, status, None)
                .await
                .unwrap();
        }
        infrastructure::repo::campaigns::set_enrollment_state(
            &mut tx,
            enrollment,
            "active",
            Some(first_step),
            Some(chrono::Utc::now()),
            None,
        )
        .await
        .unwrap();
        let basis = infrastructure::repo::campaigns::valid_marketing_basis(&mut tx, address)
            .await
            .unwrap()
            .unwrap();
        let first_outbound = infrastructure::repo::campaigns::insert_step_outbound(
            &mut tx,
            world.mail_account_id,
            channel,
            identity,
            address,
            Some(basis),
            launch,
            legacy_revision,
            enrollment,
            first_step,
            world.admin.principal_id,
            "sender@example.test",
            "legacy-flow@rcpt.dev",
            "First",
            "first",
            None,
            &[1_u8; 32],
        )
        .await
        .unwrap();
        let follow_up_outbound = infrastructure::repo::campaigns::insert_step_outbound(
            &mut tx,
            world.mail_account_id,
            channel,
            identity,
            address,
            Some(basis),
            launch,
            legacy_revision,
            enrollment,
            follow_up_step,
            world.admin.principal_id,
            "sender@example.test",
            "legacy-flow@rcpt.dev",
            "Follow up",
            "follow up",
            None,
            &[2_u8; 32],
        )
        .await
        .unwrap();
        sqlx::query(
            "UPDATE outbound_messages SET status='accepted', accepted_at=now()
              WHERE workspace_id=$1 AND id=$2",
        )
        .bind(world.workspace_id)
        .bind(first_outbound)
        .execute(tx.conn())
        .await
        .unwrap();
        infrastructure::repo::campaigns::queue_outbound_at(
            &mut tx,
            follow_up_outbound,
            chrono::Utc::now(),
        )
        .await
        .unwrap();
        let dispatch_job_id = infrastructure::repo::campaigns::enqueue_dedup(
            &mut tx,
            "sending.dispatch_outbound",
            json!({"outbound_message_id": follow_up_outbound}),
            chrono::Utc::now(),
            &infrastructure::repo::campaigns::dispatch_dedupe_key(follow_up_outbound),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        (
            legacy_revision,
            enrollment,
            first_outbound,
            follow_up_outbound,
            dispatch_job_id,
        )
    };

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    application::campaigns::schedule_next_step(
        &mut tx,
        world.system.principal_id,
        dispatch_job_id,
        enrollment_id,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let state: (String, Option<String>) = sqlx::query_as(
        "SELECT status, stop_reason FROM campaign_enrollments WHERE workspace_id=$1 AND id=$2",
    )
    .bind(world.workspace_id)
    .bind(enrollment_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let follow_up_status: (String, Option<String>) = sqlx::query_as(
        "SELECT status, failure_code FROM outbound_messages WHERE workspace_id=$1 AND id=$2",
    )
    .bind(world.workspace_id)
    .bind(follow_up_outbound)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let first_status: String =
        sqlx::query_scalar("SELECT status FROM outbound_messages WHERE workspace_id=$1 AND id=$2")
            .bind(world.workspace_id)
            .bind(first_outbound)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    let queued_jobs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE workspace_id=$1 AND kind='sending.dispatch_outbound'
          AND dedupe_key=$2 AND state IN ('queued','retry_wait','running')",
    )
    .bind(world.workspace_id)
    .bind(infrastructure::repo::campaigns::dispatch_dedupe_key(
        follow_up_outbound,
    ))
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let retained_steps: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM campaign_steps WHERE workspace_id=$1 AND revision_id=$2",
    )
    .bind(world.workspace_id)
    .bind(legacy_revision)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(state.0, "stopped");
    assert_eq!(state.1.as_deref(), Some("follow_up_automation_disabled"));
    assert_eq!(follow_up_status.0, "canceled");
    assert_eq!(follow_up_status.1, None);
    assert_eq!(queued_jobs, 0);
    assert_eq!(retained_steps, 2);
    assert_eq!(first_status, "accepted");
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn revision_requires_if_match() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (admin, csrf) = login(&app, world.admin.email.as_deref().unwrap()).await;
    let db = &app.state.db;
    let (_i, channel) = marketing_channel(db, &world).await;
    let tv = template_version(db, &world, "s", "b", json!({})).await;
    let (campaign, _r1) =
        create_campaign(&app, &admin, &csrf, &world, channel, tv, "rev-camp").await;

    let body = json!({
        "delivery_channel_id": channel,
        "timezone": "UTC",
        "schedule_rules": {"weekdays": [1], "window_start": "09:00", "window_end": "18:00"},
        "steps": [{"step_order": 1, "template_version_id": tv, "delay_hours": 0}]
    });
    // Missing If-Match → 428.
    let res = post(
        &app,
        &admin,
        &csrf,
        &format!("/api/v1/campaigns/{campaign}/revisions"),
        body.clone(),
        None,
        None,
    )
    .await;
    let p = TestApp::problem(res, StatusCode::PRECONDITION_REQUIRED).await;
    assert_eq!(p["code"], "VERSION_PRECONDITION_REQUIRED");

    // Stale If-Match → 412.
    let res = post(
        &app,
        &admin,
        &csrf,
        &format!("/api/v1/campaigns/{campaign}/revisions"),
        body.clone(),
        None,
        Some(99),
    )
    .await;
    let p = TestApp::problem(res, StatusCode::PRECONDITION_FAILED).await;
    assert_eq!(p["code"], "VERSION_MISMATCH");

    // Correct If-Match → 201 revision_no 2.
    let res = post(
        &app,
        &admin,
        &csrf,
        &format!("/api/v1/campaigns/{campaign}/revisions"),
        body,
        None,
        Some(1),
    )
    .await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let data = TestApp::data(res).await;
    assert_eq!(data["revision_no"], 2);

    let detail =
        TestApp::data(get(&app, &admin, &format!("/api/v1/campaigns/{campaign}")).await).await;
    let revision = detail["revisions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|revision| revision["revision_no"] == 2)
        .unwrap();
    assert_eq!(revision["steps"].as_array().unwrap().len(), 1);
    assert_eq!(revision["steps"][0]["template_version_id"], tv.to_string());
    assert_eq!(revision["steps"][0]["step_order"], 1);
    assert_eq!(revision["steps"][0]["delay_hours"], 0);
}

#[tokio::test]
async fn launch_approval_request_and_pause_cancel_flow() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (admin, acsrf) = login(&app, world.admin.email.as_deref().unwrap()).await;
    let (manager, mcsrf) = login(&app, world.manager.email.as_deref().unwrap()).await;
    let db = &app.state.db;
    let (_i, channel) = marketing_channel(db, &world).await;
    let tv = template_version(db, &world, "s", "b", json!({})).await;
    let addr = recipient(db, &world, "pause-me", true).await;
    let (campaign, revision) =
        create_campaign(&app, &admin, &acsrf, &world, channel, tv, "pc-camp").await;
    let (launch, job) = create_launch(
        &app,
        &admin,
        &acsrf,
        campaign,
        revision,
        &[addr],
        "pc-launch",
    )
    .await;
    let manifest_hex = freeze(db, job, &world, launch).await;

    // POST approval-requests with a wrong expected hash → SNAPSHOT_MISMATCH.
    let res = post(
        &app,
        &admin,
        &acsrf,
        &format!("/api/v1/campaign-launches/{launch}/approval-requests"),
        json!({"expected_manifest_hash": "00".repeat(32)}),
        Some("bad-hash"),
        None,
    )
    .await;
    let p = TestApp::problem(res, StatusCode::CONFLICT).await;
    assert_eq!(p["code"], "SNAPSHOT_MISMATCH");

    // Correct hash → returns the request freeze already created (200,
    // not a second request — approval_launch_active_uq).
    let res = post(
        &app,
        &admin,
        &acsrf,
        &format!("/api/v1/campaign-launches/{launch}/approval-requests"),
        json!({"expected_manifest_hash": manifest_hex, "reason": "manual re-check"}),
        Some("req-1"),
        None,
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK, "live request is returned");
    let data = TestApp::data(res).await;
    assert_eq!(data["status"], "pending");
    let request_id: Uuid = data["approval_request_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    // Approve → activate → running.
    let res = post(
        &app,
        &manager,
        &mcsrf,
        &format!("/api/v1/approvals/{request_id}/decisions"),
        json!({"decision": "approved", "expected_snapshot_hash": manifest_hex}),
        Some("pc-dec"),
        Some(1),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let data = TestApp::data(res).await;
    assert_eq!(data["execution_status"], "ready");
    assert!(data["job_id"].is_null());
    let approved =
        TestApp::data(get(&app, &admin, &format!("/api/v1/campaign-launches/{launch}")).await)
            .await;
    let job = execute_campaign_launch(
        &app,
        &admin,
        &acsrf,
        launch,
        &manifest_hex,
        approved["version"].as_i64().unwrap(),
        "pc-execute",
    )
    .await;
    run_activate(db, &world, job).await;
    let res = get(&app, &admin, &format!("/api/v1/campaign-launches/{launch}")).await;
    let etag_v: i64 = res_etag(&res).await;
    let detail = TestApp::data(res).await;
    assert_eq!(detail["status"], "running");

    // Pause (If-Match on the launch version).
    let res = post(
        &app,
        &admin,
        &acsrf,
        &format!("/api/v1/campaign-launches/{launch}/pause"),
        json!({"reason": "copy fix"}),
        None,
        Some(etag_v),
    )
    .await;
    let data = TestApp::data(res).await;
    assert_eq!(data["status"], "paused");
    let stopped: i64 = sql_scalar(
        db,
        world.workspace_id,
        "SELECT count(*) FROM campaign_enrollments WHERE workspace_id=$1 AND launch_id=$2 AND status='stopped'",
        launch,
    )
    .await;
    assert_eq!(stopped, 1);

    // Cancel → terminal; pending outbounds canceled too.
    let res = post(
        &app,
        &admin,
        &acsrf,
        &format!("/api/v1/campaign-launches/{launch}/cancel"),
        json!({"reason": "superseded"}),
        None,
        Some(data["version"].as_i64().unwrap()),
    )
    .await;
    let data = TestApp::data(res).await;
    assert_eq!(data["status"], "canceled");
    let pending_outbounds: i64 = sql_scalar(
        db,
        world.workspace_id,
        "SELECT count(*) FROM outbound_messages WHERE workspace_id=$1 AND launch_id=$2 AND status IN ('draft','queued')",
        launch,
    )
    .await;
    assert_eq!(
        pending_outbounds, 0,
        "pause/cancel must cancel pending sends"
    );
}

#[tokio::test]
async fn revoke_pending_campaign_approval_keeps_launch_status_forward_only() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (admin, csrf) = login(&app, world.admin.email.as_deref().unwrap()).await;
    let db = &app.state.db;
    let (_ident, channel) = marketing_channel(db, &world).await;
    let tv = template_version(db, &world, "subject", "body", json!({})).await;
    let addr = recipient(db, &world, "pending-revoke", true).await;
    let (campaign, revision) =
        create_campaign(&app, &admin, &csrf, &world, channel, tv, "pending-revoke").await;
    let (launch_id, job) = create_launch(
        &app,
        &admin,
        &csrf,
        campaign,
        revision,
        &[addr],
        "pending-revoke-launch",
    )
    .await;
    let manifest_hash = freeze(db, job, &world, launch_id).await;
    let request_id: Uuid = sql_scalar(
        db,
        world.workspace_id,
        "SELECT id FROM approval_requests WHERE workspace_id=$1 AND launch_id=$2",
        launch_id,
    )
    .await;

    let res = post(
        &app,
        &admin,
        &csrf,
        &format!("/api/v1/approvals/{request_id}/revoke"),
        json!({"reason": "withdrawn for review"}),
        None,
        Some(1),
    )
    .await;
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "pending approval revoke failed"
    );
    let data = TestApp::data(res).await;
    assert_eq!(data["status"], "revoked");

    let launch: Value = TestApp::data(
        get(
            &app,
            &admin,
            &format!("/api/v1/campaign-launches/{launch_id}"),
        )
        .await,
    )
    .await;
    assert_eq!(launch["status"], "awaiting_approval");

    // A new request can pin the unchanged manifest after the old request is revoked.
    let res = post(
        &app,
        &admin,
        &csrf,
        &format!("/api/v1/campaign-launches/{launch_id}/approval-requests"),
        json!({"expected_manifest_hash": manifest_hash, "reason": "reviewed again"}),
        Some("pending-revoke-retry"),
        None,
    )
    .await;
    assert_eq!(res.status(), StatusCode::CREATED);
}

/// ETag helper — GET launch responses carry the launch version.
async fn res_etag(res: &reqwest::Response) -> i64 {
    res.headers()
        .get("etag")
        .unwrap()
        .to_str()
        .unwrap()
        .trim_matches('"')
        .parse()
        .unwrap()
}

/// Emergency stop is fail-closed: creates report the block and approval
/// requests are refused (B.3.8 global block).
#[tokio::test]
async fn emergency_stop_blocks_approval_request() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (admin, csrf) = login(&app, world.admin.email.as_deref().unwrap()).await;
    let db = &app.state.db;
    let (_i, channel) = marketing_channel(db, &world).await;
    let tv = template_version(db, &world, "s", "b", json!({})).await;
    let addr = recipient(db, &world, "stop-me", true).await;

    let (campaign, revision) =
        create_campaign(&app, &admin, &csrf, &world, channel, tv, "stop-camp").await;
    let (launch, job) = create_launch(
        &app,
        &admin,
        &csrf,
        campaign,
        revision,
        &[addr],
        "stop-launch",
    )
    .await;
    let manifest_hex = freeze(db, job, &world, launch).await;

    // Re-enter emergency stop, then the approval request must 422.
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    sqlx::query("UPDATE workspaces SET emergency_stop = true WHERE id = $1")
        .bind(world.workspace_id)
        .execute(tx.conn())
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let res = post(
        &app,
        &admin,
        &csrf,
        &format!("/api/v1/campaign-launches/{launch}/approval-requests"),
        json!({"expected_manifest_hash": manifest_hex}),
        Some("stop-req"),
        None,
    )
    .await;
    let p = TestApp::problem(res, StatusCode::CONFLICT).await;
    assert_eq!(p["code"], "WORKSPACE_EMERGENCY_STOP");
}

#[tokio::test]
async fn preflight_reports_suppressed_and_unbased_recipients() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (admin, csrf) = login(&app, world.admin.email.as_deref().unwrap()).await;
    let db = &app.state.db;
    let (_i, channel) = marketing_channel(db, &world).await;
    let tv = template_version(db, &world, "s", "b", json!({})).await;
    let ok = recipient(db, &world, "ok", true).await;
    let no_basis = recipient(db, &world, "nobasis", false).await;
    let suppressed = recipient(db, &world, "supp", true).await;

    // Suppress the third address (manual suppression, marketing scope).
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    infrastructure::repo::compliance_core::insert_suppression(
        &mut tx,
        suppressed,
        "marketing",
        "manual",
        "manual",
        Some(world.admin.principal_id),
        None,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let (campaign, revision) =
        create_campaign(&app, &admin, &csrf, &world, channel, tv, "pf-camp").await;
    let res = post(
        &app,
        &admin,
        &csrf,
        &format!("/api/v1/campaigns/{campaign}/preflight"),
        json!({"revision_id": revision,
               "email_address_ids": [ok, no_basis, suppressed]}),
        None,
        None,
    )
    .await;
    let pf = TestApp::data(res).await;
    let recips = pf["recipients"].as_array().unwrap();
    let find = |id: Uuid| {
        recips
            .iter()
            .find(|r| r["email_address_id"] == id.to_string())
            .unwrap()
    };
    assert_eq!(find(ok)["recipient_checks_passed"], true);
    // A communication basis is reference information, not a blocker.
    assert_eq!(find(no_basis)["recipient_checks_passed"], true);
    assert!(
        !find(no_basis)["reason_codes"]
            .as_array()
            .unwrap()
            .contains(&json!("SEND_BASIS_UNVERIFIED"))
    );
    assert!(
        find(suppressed)["reason_codes"]
            .as_array()
            .unwrap()
            .contains(&json!("RECIPIENT_SUPPRESSED"))
    );
}

#[tokio::test]
async fn campaign_dispatch_rechecks_launch_approval_membership_and_approver_permission() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (admin, admin_csrf) = login(&app, world.admin.email.as_deref().unwrap()).await;
    let (manager, manager_csrf) = login(&app, world.manager.email.as_deref().unwrap()).await;
    let db = &app.state.db;
    let (_identity, channel) = marketing_channel(db, &world).await;
    let template =
        template_version(db, &world, "approval subject", "approved body", json!({})).await;
    let address = recipient(db, &world, "approved-campaign", true).await;
    let (campaign, revision) = create_campaign(
        &app,
        &admin,
        &admin_csrf,
        &world,
        channel,
        template,
        "campaign-approval",
    )
    .await;
    let (launch, freeze_job) = create_launch(
        &app,
        &admin,
        &admin_csrf,
        campaign,
        revision,
        &[address],
        "campaign-approval-launch",
    )
    .await;
    let manifest_hash = freeze(db, freeze_job, &world, launch).await;

    let request = post(
        &app,
        &admin,
        &admin_csrf,
        &format!("/api/v1/campaign-launches/{launch}/approval-requests"),
        json!({"expected_manifest_hash": manifest_hash}),
        Some("campaign-approval-request"),
        None,
    )
    .await;
    assert_eq!(request.status(), StatusCode::OK);
    let request_data = TestApp::data(request).await;
    let request_id: Uuid = request_data["approval_request_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    let decision = post(
        &app,
        &manager,
        &manager_csrf,
        &format!("/api/v1/approvals/{request_id}/decisions"),
        json!({"decision": "approved", "expected_snapshot_hash": manifest_hash}),
        Some("campaign-approval-decision"),
        Some(1),
    )
    .await;
    assert_eq!(decision.status(), StatusCode::OK);
    let decision_data = TestApp::data(decision).await;
    assert_eq!(decision_data["execution_status"], "ready");
    assert!(decision_data["job_id"].is_null());
    let approved =
        TestApp::data(get(&app, &admin, &format!("/api/v1/campaign-launches/{launch}")).await)
            .await;
    let activation_job = execute_campaign_launch(
        &app,
        &admin,
        &admin_csrf,
        launch,
        &manifest_hash,
        approved["version"].as_i64().unwrap(),
        "campaign-approval-execute",
    )
    .await;
    run_activate(db, &world, activation_job).await;

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let outbound: Uuid = sqlx::query_scalar(
        "SELECT id FROM outbound_messages
          WHERE workspace_id = $1 AND launch_id = $2 AND status = 'queued'
          ORDER BY created_at, id LIMIT 1",
    )
    .bind(world.workspace_id)
    .bind(launch)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let check = |data: &Value, name: &str| {
        data["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["name"] == name)
            .cloned()
            .expect("expected preflight check")
    };
    let preflight_path = format!("/api/v1/outbound-messages/{outbound}/preflight");
    let valid_res = post(
        &app,
        &admin,
        &admin_csrf,
        &preflight_path,
        json!({}),
        None,
        None,
    )
    .await;
    assert_eq!(valid_res.status(), StatusCode::OK);
    let valid = TestApp::data(valid_res).await;
    for name in ["approval_state", "approval_hash", "approver_permission"] {
        assert_eq!(
            check(&valid, name)["passed"],
            true,
            "{name} should pass for the approved frozen campaign"
        );
    }

    // The approver must retain an independent, current mailbox approval grant.
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    sqlx::query(
        "UPDATE mail_account_grants SET can_approve = false
          WHERE workspace_id = $1 AND mail_account_id = $2 AND principal_id = $3",
    )
    .bind(world.workspace_id)
    .bind(world.mail_account_id)
    .bind(world.manager.principal_id)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let denied_res = post(
        &app,
        &admin,
        &admin_csrf,
        &preflight_path,
        json!({}),
        None,
        None,
    )
    .await;
    let denied = TestApp::data(denied_res).await;
    assert_eq!(check(&denied, "approval_state")["passed"], true);
    assert_eq!(check(&denied, "approval_hash")["passed"], true);
    assert_eq!(check(&denied, "approver_permission")["passed"], false);
    assert_eq!(
        check(&denied, "approver_permission")["code"],
        "APPROVER_PERMISSION_REVOKED"
    );

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    sqlx::query(
        "UPDATE mail_account_grants SET can_approve = true
          WHERE workspace_id = $1 AND mail_account_id = $2 AND principal_id = $3",
    )
    .bind(world.workspace_id)
    .bind(world.mail_account_id)
    .bind(world.manager.principal_id)
    .execute(tx.conn())
    .await
    .unwrap();
    let _ = infrastructure::repo::compliance_core::lock_email_address(&mut tx, address)
        .await
        .unwrap();
    let frozen = infrastructure::repo::sending::get_outbound(&mut tx, outbound)
        .await
        .unwrap()
        .unwrap();

    let mut changed_recipient = frozen.clone();
    changed_recipient.to_email_address_id = Uuid::new_v4();
    let membership_checks =
        application::sending::policy::evaluate(&mut tx, &changed_recipient, None)
            .await
            .unwrap();
    let membership_hash = membership_checks
        .iter()
        .find(|item| item.name == "approval_hash")
        .unwrap();
    assert!(!membership_hash.passed);
    assert_eq!(membership_hash.code, Some("SNAPSHOT_MISMATCH"));

    let mut changed_payload = frozen;
    changed_payload.body_text.push_str(" changed after freeze");
    let payload_checks = application::sending::policy::evaluate(&mut tx, &changed_payload, None)
        .await
        .unwrap();
    let payload_hash = payload_checks
        .iter()
        .find(|item| item.name == "approval_hash")
        .unwrap();
    assert!(!payload_hash.passed);
    assert_eq!(payload_hash.code, Some("SNAPSHOT_MISMATCH"));
    tx.commit().await.unwrap();

    // Revocation blocks an already-queued campaign message at preflight.
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let version: i64 = sqlx::query_scalar(
        "SELECT version FROM approval_requests WHERE workspace_id = $1 AND id = $2",
    )
    .bind(world.workspace_id)
    .bind(request_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(
        infrastructure::repo::approvals::set_request_status(
            &mut tx, request_id, "revoked", version
        )
        .await
        .unwrap(),
        1
    );
    tx.commit().await.unwrap();
    let revoked_res = post(
        &app,
        &admin,
        &admin_csrf,
        &preflight_path,
        json!({}),
        None,
        None,
    )
    .await;
    let revoked = TestApp::data(revoked_res).await;
    assert_eq!(check(&revoked, "approval_state")["passed"], false);
    assert_eq!(
        check(&revoked, "approval_state")["code"],
        "APPROVAL_REVOKED"
    );
    assert_eq!(check(&revoked, "approval_hash")["passed"], false);
}

#[tokio::test]
async fn preflight_blocks_a_required_booking_url_without_a_meeting_binding() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (admin, csrf) = login(&app, world.admin.email.as_deref().unwrap()).await;
    let db = &app.state.db;
    let (_ident, channel) = marketing_channel(db, &world).await;
    let schema = json!({
        "type": "object",
        "properties": {
            "meeting.booking_url": {"type": "string", "format": "uri"}
        },
        "required": ["meeting.booking_url"],
        "additionalProperties": false
    });
    let tv = template_version(
        db,
        &world,
        "Book a meeting",
        "Choose a time: {{meeting.booking_url}}",
        schema,
    )
    .await;
    let recipient = recipient(db, &world, "binding-required", true).await;
    let (campaign, revision) =
        create_campaign(&app, &admin, &csrf, &world, channel, tv, "binding-required").await;

    let res = post(
        &app,
        &admin,
        &csrf,
        &format!("/api/v1/campaigns/{campaign}/preflight"),
        json!({"revision_id": revision, "email_address_ids": [recipient]}),
        None,
        None,
    )
    .await;
    let preflight = TestApp::data(res).await;
    assert!(
        preflight["global_blocking_reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason == "MEETING_TYPE_BINDING_REQUIRED")
    );
    assert!(
        preflight["recipients"][0]["reason_codes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason == "MEETING_TYPE_BINDING_REQUIRED")
    );

    let mut invalid_binding = campaign_body(world.mail_account_id, channel, tv);
    invalid_binding["schedule_rules"]["meeting_type_id"] = json!(Uuid::new_v4());
    let res = post(
        &app,
        &admin,
        &csrf,
        "/api/v1/campaigns",
        invalid_binding,
        Some("invalid-meeting-binding"),
        None,
    )
    .await;
    let problem = TestApp::problem(res, StatusCode::NOT_FOUND).await;
    assert_eq!(problem["code"], "NOT_FOUND");
}

#[tokio::test]
async fn approved_retired_provider_approval_is_read_only_history() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (admin, acsrf) = login(&app, world.admin.email.as_deref().unwrap()).await;
    let (manager, mcsrf) = login(&app, world.manager.email.as_deref().unwrap()).await;
    let db = &app.state.db;
    let (_, channel) = marketing_channel(db, &world).await;
    let tv = template_version(
        db,
        &world,
        "Retired history",
        "Hello {{recipient.email}}",
        json!({
            "type": "object",
            "properties": {"recipient.email": {"type": "string"}},
            "required": ["recipient.email"]
        }),
    )
    .await;
    let address = recipient(db, &world, "retired-history", true).await;
    let (campaign, revision) = create_campaign(
        &app,
        &admin,
        &acsrf,
        &world,
        channel,
        tv,
        "retired-history-campaign",
    )
    .await;
    let (launch, freeze_job) = create_launch(
        &app,
        &admin,
        &acsrf,
        campaign,
        revision,
        &[address],
        "retired-history-launch",
    )
    .await;
    let manifest_hash = freeze(db, freeze_job, &world, launch).await;
    let request_id: Uuid = sql_scalar(
        db,
        world.workspace_id,
        "SELECT id FROM approval_requests WHERE workspace_id=$1 AND launch_id=$2",
        launch,
    )
    .await;

    let decision = post(
        &app,
        &manager,
        &mcsrf,
        &format!("/api/v1/approvals/{request_id}/decisions"),
        json!({"decision":"approved","expected_snapshot_hash":manifest_hash}),
        Some("retired-history-approve"),
        Some(1),
    )
    .await;
    assert_eq!(decision.status(), StatusCode::OK);
    let approval = TestApp::data(decision).await;
    assert_eq!(approval["execution_status"], "ready");
    assert!(approval["job_id"].is_null());
    let approved =
        TestApp::data(get(&app, &admin, &format!("/api/v1/campaign-launches/{launch}")).await)
            .await;
    let _activation_job = execute_campaign_launch(
        &app,
        &admin,
        &acsrf,
        launch,
        &manifest_hash,
        approved["version"].as_i64().unwrap(),
        "retired-history-execute",
    )
    .await;

    // Simulate the retained post-upgrade state: connection/channel disabled,
    // launch canceled, and the approval decision preserved as history.
    let owner = Db::connect(
        &std::env::var("DATABASE_URL").expect("DATABASE_URL must be set"),
        None,
    )
    .await
    .unwrap();
    let mut tx = owner.begin(world.workspace_id).await.unwrap();
    let retired_connection: Uuid = sqlx::query_scalar(
        "INSERT INTO integration_connections
            (workspace_id, provider, region, secret_ref, scopes, status, connected_by)
         VALUES ($1, 'zoho_campaigns', 'com', $2, ARRAY[]::text[], 'disabled', $3)
         RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(format!("retired-test:{}", Uuid::new_v4()))
    .bind(world.admin.principal_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    sqlx::query("INSERT INTO marketing_connections (id, workspace_id) VALUES ($1, $2)")
        .bind(retired_connection)
        .bind(world.workspace_id)
        .execute(tx.conn())
        .await
        .unwrap();
    sqlx::query(
        "UPDATE delivery_channels
            SET provider='zoho_campaigns', connection_id=$3,
                enabled=false, policy_status='blocked', version=version+1
          WHERE workspace_id=$1 AND id=$2",
    )
    .bind(world.workspace_id)
    .bind(channel)
    .bind(retired_connection)
    .execute(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE campaign_launches
            SET status='canceled', version=version+1
          WHERE workspace_id=$1 AND id=$2",
    )
    .bind(world.workspace_id)
    .bind(launch)
    .execute(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE jobs
            SET state='canceled', last_error_code='PROVIDER_RETIRED', finished_at=now()
          WHERE workspace_id=$1 AND kind='campaigns.activate_launch'
            AND payload->>'campaign_launch_id'=$2::uuid::text",
    )
    .bind(world.workspace_id)
    .bind(launch.to_string())
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let detail_response = get(&app, &admin, &format!("/api/v1/approvals/{request_id}")).await;
    assert_eq!(detail_response.status(), StatusCode::OK);
    let detail = TestApp::data(detail_response).await;
    assert_eq!(detail["status"], "approved");
    assert_eq!(detail["retired_provider"], true);
    assert_eq!(detail["can_decide"], false);

    let version: i64 = sql_scalar(
        db,
        world.workspace_id,
        "SELECT version FROM approval_requests WHERE workspace_id=$1 AND id=$2",
        request_id,
    )
    .await;
    let revoke = post(
        &app,
        &admin,
        &acsrf,
        &format!("/api/v1/approvals/{request_id}/revoke"),
        json!({"reason":"legacy provider retirement"}),
        Some("retired-history-revoke"),
        Some(version),
    )
    .await;
    let problem = TestApp::problem(revoke, StatusCode::CONFLICT).await;
    assert_eq!(problem["code"], "CONFLICT");

    let status: String = sql_scalar(
        db,
        world.workspace_id,
        "SELECT status FROM approval_requests WHERE workspace_id=$1 AND id=$2",
        request_id,
    )
    .await;
    assert_eq!(status, "approved");
    let job_state: String = {
        let mut tx = db.begin(world.workspace_id).await.unwrap();
        let state = sqlx::query_scalar(
            "SELECT state FROM jobs
              WHERE workspace_id=$1 AND kind='campaigns.activate_launch'
                AND payload->>'campaign_launch_id'=$2::uuid::text",
        )
        .bind(world.workspace_id)
        .bind(launch.to_string())
        .fetch_one(tx.conn())
        .await
        .unwrap();
        tx.rollback().await.unwrap();
        state
    };
    assert_eq!(job_state, "canceled");
}
