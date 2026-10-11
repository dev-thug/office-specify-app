//! Operations/agents REST coverage for the safety boundaries in Task 11.

use std::sync::Arc;

use api::{AppState, testing::TestApp};
use chrono::{Duration, Utc};
use contracts::agents::{AgentRunBudget, CancelAgentRunRequest, CreateAgentRunRequest};
use contracts::{Scope, ScopeSet};
use infrastructure::{
    config::Config,
    db::{Db, ROLE_API},
    repo::compliance::test_fixtures as fixtures,
    testing::{TestWorld, ctx_for, ctx_for_credential, seed_workspace},
};
use reqwest::StatusCode;
use serde_json::json;
use uuid::Uuid;

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

async fn login_admin(app: &TestApp, world: &TestWorld) -> String {
    let email = world.admin.email.as_deref().unwrap();
    let response = app
        .login(email, infrastructure::testing::TEST_PASSWORD)
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    TestApp::csrf_token_from(&response).expect("login sets CSRF cookie")
}

async fn create_run(app: &TestApp, world: &TestWorld, budget: AgentRunBudget) -> Uuid {
    let ctx = ctx_for(world, &world.admin, "test-agent-run-create");
    let response = application::agents::create_run(
        &app.state.db,
        &ctx,
        &CreateAgentRunRequest {
            agent_principal_id: world.agent.principal_id,
            objective: "Test an approved agent run".into(),
            scope: None,
            budget: Some(budget),
        },
    )
    .await
    .unwrap();
    response.id
}

fn agent_ctx(
    world: &TestWorld,
    credential_id: Uuid,
    run_id: Option<Uuid>,
    request_id: &str,
) -> contracts::Ctx {
    let mut ctx = ctx_for_credential(
        world,
        &world.agent,
        credential_id,
        ScopeSet::of([Scope::AgentsRun, Scope::CrmRead]),
        request_id,
    );
    ctx.agent_run_id = run_id;
    ctx
}

async fn insert_agent_credential(app: &TestApp, world: &TestWorld) -> Uuid {
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO service_credentials
            (workspace_id, principal_id, name, key_prefix, token_hash, scopes, expires_at)
         VALUES ($1, $2, 'test MCP credential', 'osk_test', $3, $4, now() + interval '1 hour')
         RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(world.agent.principal_id)
    .bind(infrastructure::crypto::sha256_bytes(
        Uuid::new_v4().as_bytes(),
    ))
    .bind(vec!["agents.run".to_string(), "crm.read".to_string()])
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    id
}

async fn insert_job(app: &TestApp, workspace_id: Uuid, state: &str) -> Uuid {
    insert_job_for(app, workspace_id, state, "sending.dispatch_outbound", None).await
}

async fn insert_job_for(
    app: &TestApp,
    workspace_id: Uuid,
    state: &str,
    kind: &str,
    requested_by: Option<Uuid>,
) -> Uuid {
    let mut tx = app.state.db.begin(workspace_id).await.unwrap();
    let id = sqlx::query_scalar(
        "INSERT INTO jobs
            (workspace_id, kind, payload_version, payload, dedupe_key, state,
             external_effect, last_error_code, requested_by)
         VALUES ($1, $2, 1,
                 '{\"outbound_message_id\":\"10000000-0000-4000-8000-000000000001\"}',
                 $3, $4, true, 'TIMEOUT', $5)
         RETURNING id",
    )
    .bind(workspace_id)
    .bind(kind)
    .bind(format!("test:{}", Uuid::new_v4()))
    .bind(state)
    .bind(requested_by)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    id
}

async fn insert_dashboard_thread(
    tx: &mut infrastructure::db::Tx<'_>,
    workspace_id: Uuid,
    mail_account_id: Uuid,
    assigned_principal_id: Uuid,
    status: &str,
    suffix: &str,
) -> Uuid {
    let last_inbound = Utc::now() - Duration::minutes(15);
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO email_threads
            (workspace_id, mail_account_id, assigned_principal_id, status, updated_at)
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(workspace_id)
    .bind(mail_account_id)
    .bind(assigned_principal_id)
    .bind(status)
    .bind(last_inbound)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO email_messages
            (workspace_id, mail_account_id, thread_id, direction, message_at,
             observed_at, subject, body_text)
         VALUES ($1, $2, $3, 'inbound', $4, $4, $5, $6)",
    )
    .bind(workspace_id)
    .bind(mail_account_id)
    .bind(id)
    .bind(last_inbound)
    .bind(format!("PRIVATE subject {suffix}"))
    .bind(format!("PRIVATE body {suffix}"))
    .execute(tx.conn())
    .await
    .unwrap();
    id
}

async fn insert_dashboard_mailbox(tx: &mut infrastructure::db::Tx<'_>, world: &TestWorld) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO mail_accounts
            (workspace_id, connection_id, provider_account_id, primary_address, name,
             status, last_synced_at)
         VALUES ($1, $2, $3, 'hidden@example.test', 'Hidden Mailbox',
                 'reauth_required', now() - interval '1 hour')
         RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(world.connection_id)
    .bind(format!("hidden-dashboard-{}", Uuid::new_v4()))
    .fetch_one(tx.conn())
    .await
    .unwrap()
}

async fn insert_dashboard_booking(app: &TestApp, world: &TestWorld, host_id: Uuid) -> Uuid {
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let calendar_connection_id: Uuid = sqlx::query_scalar(
        "INSERT INTO integration_connections
            (workspace_id, provider, region, secret_ref, status, connected_by)
         VALUES ($1, 'zoho_calendar', 'us', $2, 'active', $3) RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(format!("dashboard-calendar-{}", Uuid::new_v4()))
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
    .bind(host_id)
    .bind(calendar_connection_id)
    .bind(format!("dashboard-calendar-id-{}", Uuid::new_v4()))
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let meeting_type_id: Uuid = sqlx::query_scalar(
        "INSERT INTO meeting_types
            (workspace_id, host_principal_id, calendar_connection_id, name)
         VALUES ($1, $2, $3, 'Dashboard Test Meeting') RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(host_id)
    .bind(calendar_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let booking_link_id: Uuid = sqlx::query_scalar(
        "INSERT INTO booking_links
            (workspace_id, meeting_type_id, host_principal_id, token_hash)
         VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(meeting_type_id)
    .bind(host_id)
    .bind(infrastructure::crypto::sha256_bytes(
        Uuid::new_v4().as_bytes(),
    ))
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let guest_email_id = fixtures::insert_email_address(
        &mut tx,
        &format!("dashboard-{}@example.test", Uuid::new_v4()),
    )
    .await
    .unwrap();
    let starts_at: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
        "SELECT (date_trunc('day', now() AT TIME ZONE default_timezone)
                    + interval '12 hours') AT TIME ZONE default_timezone
           FROM workspaces WHERE id = $1",
    )
    .bind(world.workspace_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let ends_at = starts_at + Duration::minutes(30);
    let booking_id: Uuid = sqlx::query_scalar(
        "INSERT INTO bookings
            (workspace_id, booking_link_id, host_principal_id, guest_email_address_id,
             guest_name, guest_timezone, starts_at, ends_at, occupied_start, occupied_end,
             status, provider_event_id)
         VALUES ($1, $2, $3, $4, 'Dashboard Guest', 'Asia/Seoul', $5, $6, $5, $6,
                 'confirmed', $7) RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(booking_link_id)
    .bind(host_id)
    .bind(guest_email_id)
    .bind(starts_at)
    .bind(ends_at)
    .bind(format!("dashboard-event-{}", Uuid::new_v4()))
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    booking_id
}

async fn insert_dashboard_booking_approval(
    app: &TestApp,
    world: &TestWorld,
    requested_by: Uuid,
) -> Uuid {
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let change_id: Uuid = sqlx::query_scalar(
        "INSERT INTO booking_changes
            (workspace_id, host_principal_id, kind, proposed_data, requested_by, status)
         VALUES ($1, $2, 'create', '{\"timezone\":\"Asia/Seoul\"}', $3, 'proposed')
         RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(world.member_a.principal_id)
    .bind(requested_by)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let hash = [0x56_u8; 32];
    let approval_id = infrastructure::repo::approvals::insert_request(
        &mut tx,
        requested_by,
        infrastructure::repo::approvals::ApprovalTarget::BookingChange(change_id),
        &hash,
        Utc::now() + Duration::hours(1),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    approval_id
}

fn dashboard_queue_count(body: &serde_json::Value, key: &str) -> Option<i64> {
    body["data"]["queues"]
        .as_array()?
        .iter()
        .find(|queue| queue["key"] == key)?["count"]
        .as_i64()
}

#[tokio::test]
async fn t11_unknown_job_retry_is_rejected_without_changing_state() {
    let (app, world) = test_app().await;
    let csrf = login_admin(&app, &world).await;
    let job_id = insert_job(&app, world.workspace_id, "unknown").await;

    let response = app
        .client
        .post(app.url(&format!("/api/v1/jobs/{job_id}/retry-requests")))
        .header("x-csrf-token", csrf)
        .header("idempotency-key", "unknown-must-not-retry")
        .json(&json!({"reason": "provider result needs reconciliation"}))
        .send()
        .await
        .unwrap();
    let problem = TestApp::problem(response, StatusCode::CONFLICT).await;
    assert_eq!(problem["code"], "PROVIDER_SUBMISSION_OUTCOME_UNKNOWN");

    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let state: String = sqlx::query_scalar("SELECT state FROM jobs WHERE id = $1")
        .bind(job_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(state, "unknown");
}

#[tokio::test]
async fn t29_database_job_queue_remains_visible_without_redis() {
    let (app, world) = test_app().await;
    let _csrf = login_admin(&app, &world).await;
    let job_id = insert_job(&app, world.workspace_id, "queued").await;
    let response = app.get(&format!("/api/v1/jobs/{job_id}")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["data"]["id"], job_id.to_string());
    assert_eq!(body["data"]["state"], "queued");
}

#[tokio::test]
async fn t29_job_collection_uses_signed_filters_and_requester_visibility() {
    let (app, world) = test_app().await;
    let _csrf = login_admin(&app, &world).await;
    let admin_queued = insert_job_for(
        &app,
        world.workspace_id,
        "queued",
        "sending.dispatch_outbound",
        Some(world.admin.principal_id),
    )
    .await;
    let member_queued = insert_job_for(
        &app,
        world.workspace_id,
        "queued",
        "imports.validate",
        Some(world.member_a.principal_id),
    )
    .await;
    insert_job_for(
        &app,
        world.workspace_id,
        "dead",
        "imports.validate",
        Some(world.admin.principal_id),
    )
    .await;

    let first = app.get("/api/v1/jobs?state=queued&limit=1").await;
    assert_eq!(first.status(), StatusCode::OK);
    let first: serde_json::Value = first.json().await.unwrap();
    assert_eq!(first["data"].as_array().unwrap().len(), 1);
    assert_eq!(first["page"]["has_more"], true);
    assert!(first["page"]["next_cursor"].as_str().is_some());
    assert!(first["data"][0].get("payload").is_none());
    assert!(first["data"][0].get("result_refs").is_none());
    let first_id = first["data"][0]["id"].as_str().unwrap().to_string();

    let cursor = first["page"]["next_cursor"].as_str().unwrap();
    let second = app
        .get(&format!(
            "/api/v1/jobs?state=queued&limit=1&cursor={cursor}"
        ))
        .await;
    assert_eq!(second.status(), StatusCode::OK);
    let second: serde_json::Value = second.json().await.unwrap();
    assert_eq!(second["data"].as_array().unwrap().len(), 1);
    let second_id = second["data"][0]["id"].as_str().unwrap().to_string();
    assert_ne!(first_id, second_id);
    let admin_id = admin_queued.to_string();
    let member_id = member_queued.to_string();
    assert!(first_id == admin_id || second_id == admin_id);
    assert!(first_id == member_id || second_id == member_id);
    assert_eq!(second["page"]["has_more"], false);

    let changed_filter = app
        .get(&format!(
            "/api/v1/jobs?state=queued&kind=imports.validate&limit=1&cursor={cursor}"
        ))
        .await;
    let problem = TestApp::problem(changed_filter, StatusCode::BAD_REQUEST).await;
    assert_eq!(problem["code"], "VALIDATION_FAILED");

    let kind_filter = app.get("/api/v1/jobs?kind=imports.validate&limit=20").await;
    assert_eq!(kind_filter.status(), StatusCode::OK);
    let kind_filter: serde_json::Value = kind_filter.json().await.unwrap();
    assert_eq!(kind_filter["data"].as_array().unwrap().len(), 2);

    let invalid_state = app.get("/api/v1/jobs?state=not-a-job-state").await;
    let problem = TestApp::problem(invalid_state, StatusCode::BAD_REQUEST).await;
    assert_eq!(problem["code"], "VALIDATION_FAILED");
    let invalid_kind = app.get("/api/v1/jobs?kind=../not-safe").await;
    let problem = TestApp::problem(invalid_kind, StatusCode::BAD_REQUEST).await;
    assert_eq!(problem["code"], "VALIDATION_FAILED");

    let db = app.state.db.clone();
    let member_app = TestApp::spawn_with_state(AppState::with_fake_providers(
        db,
        Arc::new(Config::test()),
        None,
    ))
    .await;
    let member_email = world.member_a.email.as_deref().unwrap();
    let member_login = member_app
        .login(member_email, infrastructure::testing::TEST_PASSWORD)
        .await;
    assert_eq!(member_login.status(), StatusCode::OK);
    let member_page = member_app.get("/api/v1/jobs?state=queued&limit=20").await;
    assert_eq!(member_page.status(), StatusCode::OK);
    let member_page: serde_json::Value = member_page.json().await.unwrap();
    assert_eq!(member_page["data"].as_array().unwrap().len(), 1);
    assert_eq!(member_page["data"][0]["id"], member_queued.to_string());
}

#[tokio::test]
async fn t30_restore_lock_release_requires_reason_and_records_no_raw_reason() {
    let (app, world) = test_app().await;
    let csrf = login_admin(&app, &world).await;
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    infrastructure::repo::operations::set_send_workers_locked(&mut tx, true)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let before = app.get("/api/v1/operations/alerts").await;
    assert_eq!(before.status(), StatusCode::OK);
    let before: serde_json::Value = before.json().await.unwrap();
    assert_eq!(before["data"]["send_workers_locked"], true);

    let reason = "reconciled provider history through checkpoint 2026-10-09";
    let released = app
        .client
        .post(app.url("/api/v1/operations/send-workers-lock/release"))
        .header("x-csrf-token", &csrf)
        .header("idempotency-key", "restore-reconcile-release")
        .json(&json!({"reason": reason}))
        .send()
        .await
        .unwrap();
    assert_eq!(released.status(), StatusCode::OK);
    let released: serde_json::Value = released.json().await.unwrap();
    assert_eq!(released["data"]["send_workers_locked"], false);

    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let audit: serde_json::Value = sqlx::query_scalar(
        "SELECT details FROM audit_logs WHERE action = 'operations.send_workers_lock_released'",
    )
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(audit.get("reason").is_none());
    assert!(audit["reason_hash"].as_str().is_some());
    assert!(audit.to_string().find(reason).is_none());
}

#[tokio::test]
async fn t13_tool_call_checks_run_credential_budget_cancellation_and_redaction() {
    let (app, world) = test_app().await;
    let run_id = create_run(
        &app,
        &world,
        AgentRunBudget {
            max_tool_calls: Some(1),
            ..Default::default()
        },
    )
    .await;
    let credential_id = insert_agent_credential(&app, &world).await;
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    sqlx::query("UPDATE agent_runs SET source_credential_id = $2 WHERE id = $1")
        .bind(run_id)
        .bind(credential_id)
        .execute(tx.conn())
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let args = json!({
        "agent_run_id": run_id,
        "company_id": "20000000-0000-4000-8000-000000000001",
        "email": "private@example.com",
        "body": "never store this tool input",
        "access_token": "never store this token"
    });
    let mut wrong_credential = agent_ctx(
        &world,
        Uuid::new_v4(),
        Some(run_id),
        "tool-wrong-credential",
    );
    let error = application::agents::begin_tool_call(
        &app.state.db,
        &mut wrong_credential,
        &args,
        "crm_search_companies",
        false,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, contracts::ErrorCode::NotFound);

    let mut agent = agent_ctx(&world, credential_id, Some(run_id), "tool-start");
    let action_id = application::agents::begin_tool_call(
        &app.state.db,
        &mut agent,
        &args,
        "crm_search_companies",
        false,
    )
    .await
    .unwrap()
    .expect("run-bound calls create an action record");
    application::agents::finish_tool_call(&app.state.db, &agent, Some(action_id), "ok")
        .await
        .unwrap();

    let mut over_budget = agent_ctx(&world, credential_id, Some(run_id), "tool-over-budget");
    let error = application::agents::begin_tool_call(
        &app.state.db,
        &mut over_budget,
        &args,
        "crm_search_companies",
        false,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, contracts::ErrorCode::RateLimited);

    let admin_ctx = ctx_for(&world, &world.admin, "test-agent-run-create");
    application::agents::cancel_run(
        &app.state.db,
        &admin_ctx,
        run_id,
        &CancelAgentRunRequest {
            reason: Some("run complete".into()),
        },
    )
    .await
    .unwrap();
    let mut canceled = agent_ctx(&world, credential_id, Some(run_id), "tool-after-cancel");
    let error = application::agents::begin_tool_call(
        &app.state.db,
        &mut canceled,
        &args,
        "crm_search_companies",
        false,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, contracts::ErrorCode::Conflict);

    let detail = application::agents::get_run_detail(&app.state.db, &admin_ctx, run_id)
        .await
        .unwrap();
    assert_eq!(detail.status, contracts::agents::AgentRunStatus::Canceled);
    assert_eq!(detail.usage["tool_calls"], 1);
    assert_eq!(detail.actions.len(), 1);
    assert_eq!(detail.actions[0].id, action_id);
    assert_eq!(
        detail.actions[0].redacted_arguments,
        json!({
            "agent_run_id": run_id,
            "company_id": "20000000-0000-4000-8000-000000000001"
        })
    );
    assert!(
        !detail.actions[0]
            .redacted_arguments
            .to_string()
            .contains("private@example.com")
    );

    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let event_payloads: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT payload FROM outbox_events
          WHERE aggregate_id = $1 AND event_type IN ('agent.tool_call.started','agent.tool_call.finished')
          ORDER BY created_at, id",
    )
    .bind(action_id)
    .fetch_all(tx.conn())
    .await
    .unwrap();
    let audit_details: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT details FROM audit_logs WHERE target_id = $1 ORDER BY occurred_at, id",
    )
    .bind(action_id)
    .fetch_all(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(event_payloads.len(), 2);
    assert!(
        event_payloads
            .iter()
            .all(|payload| payload.get("agent_action_id").is_some())
    );
    assert!(
        event_payloads
            .iter()
            .all(|payload| !payload.to_string().contains("private@example.com"))
    );
    assert!(
        audit_details
            .iter()
            .all(|details| !details.to_string().contains("never store this"))
    );
}

#[tokio::test]
async fn t13_tool_call_without_a_run_is_still_audited_without_raw_arguments() {
    let (app, world) = test_app().await;
    let credential_id = insert_agent_credential(&app, &world).await;
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let action_count_before: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM agent_actions WHERE tool_name = 'crm_search_companies'",
    )
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let mut ctx = agent_ctx(&world, credential_id, None, "tool-without-run");
    let args = json!({"email": "private@example.com", "body": "private content"});
    assert_eq!(
        application::agents::begin_tool_call(
            &app.state.db,
            &mut ctx,
            &args,
            "crm_search_companies",
            false,
        )
        .await
        .unwrap(),
        None
    );
    application::agents::finish_tool_call(&app.state.db, &ctx, None, "ok")
        .await
        .unwrap();

    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let rows: Vec<(String, serde_json::Value)> = sqlx::query_as(
        "SELECT action, details FROM audit_logs WHERE request_id = 'tool-without-run' ORDER BY occurred_at, id",
    )
    .fetch_all(tx.conn())
    .await
    .unwrap();
    let action_count_after: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM agent_actions WHERE tool_name = 'crm_search_companies'",
    )
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, "agent.tool_call.started");
    assert_eq!(rows[1].0, "agent.tool_call.finished");
    assert!(rows.iter().all(|(_, details)| {
        !details.to_string().contains("private@example.com")
            && !details.to_string().contains("private content")
    }));
    assert_eq!(action_count_after, action_count_before);
}

#[tokio::test]
async fn t13_tool_call_cannot_use_another_agents_run() {
    let (app, world) = test_app().await;
    let run_id = create_run(&app, &world, AgentRunBudget::default()).await;
    let credential_id = insert_agent_credential(&app, &world).await;
    let other_agent_id: Uuid = {
        let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
        let id = sqlx::query_scalar(
            "INSERT INTO principals (workspace_id, kind, name, role, status)
             VALUES ($1, 'agent', 'Other test agent', 'agent', 'active') RETURNING id",
        )
        .bind(world.workspace_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        tx.commit().await.unwrap();
        id
    };
    let mut other = agent_ctx(&world, credential_id, Some(run_id), "cross-agent-tool-call");
    other.principal_id = other_agent_id;
    let error = application::agents::begin_tool_call(
        &app.state.db,
        &mut other,
        &json!({"company_id": "20000000-0000-4000-8000-000000000001"}),
        "crm_search_companies",
        false,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, contracts::ErrorCode::NotFound);
    let error = application::agents::finish_run(
        &app.state.db,
        &other,
        run_id,
        &contracts::agents::FinishAgentRunRequest {
            outcome: contracts::agents::AgentRunFinishOutcome::Completed,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, contracts::ErrorCode::NotFound);

    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let actions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM agent_actions WHERE agent_run_id = $1")
            .bind(run_id)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    let denied: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_logs WHERE action = 'agent.tool_call.denied' AND request_id = 'cross-agent-tool-call'",
    )
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(actions, 0);
    assert_eq!(denied, 1);
}

#[tokio::test]
async fn t13_owner_finishes_run_only_after_actions_finish_and_repeat_is_idempotent() {
    let (app, world) = test_app().await;
    let run_id = create_run(&app, &world, AgentRunBudget::default()).await;
    let credential_id = insert_agent_credential(&app, &world).await;
    let mut ctx = agent_ctx(&world, credential_id, Some(run_id), "runner-finish");
    let action_id = application::agents::begin_tool_call(
        &app.state.db,
        &mut ctx,
        &json!({"company_id": "20000000-0000-4000-8000-000000000001"}),
        "crm_search_companies",
        true,
    )
    .await
    .unwrap()
    .unwrap();
    let request = contracts::agents::FinishAgentRunRequest {
        outcome: contracts::agents::AgentRunFinishOutcome::Completed,
    };
    let error = application::agents::finish_run(&app.state.db, &ctx, run_id, &request)
        .await
        .unwrap_err();
    assert_eq!(error.code, contracts::ErrorCode::Conflict);

    application::agents::finish_tool_call(&app.state.db, &ctx, Some(action_id), "ok")
        .await
        .unwrap();
    let finished = application::agents::finish_run(&app.state.db, &ctx, run_id, &request)
        .await
        .unwrap();
    assert_eq!(
        finished.status,
        contracts::agents::AgentRunStatus::Completed
    );
    let replay = application::agents::finish_run(&app.state.db, &ctx, run_id, &request)
        .await
        .unwrap();
    assert_eq!(replay.status, contracts::agents::AgentRunStatus::Completed);

    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let audit_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_logs WHERE action = 'agent_run.completed' AND target_id = $1",
    )
    .bind(run_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let outbox_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM outbox_events WHERE event_type = 'agent_run.completed' AND aggregate_id = $1",
    )
    .bind(run_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(audit_count, 1);
    assert_eq!(outbox_count, 1);
}

#[tokio::test]
async fn t13_admin_can_recover_finish_but_canceled_run_cannot_finish() {
    let (app, world) = test_app().await;
    let csrf = login_admin(&app, &world).await;
    let admin = ctx_for(&world, &world.admin, "admin-run-recovery");
    let recoverable = create_run(&app, &world, AgentRunBudget::default()).await;
    let recovered = app
        .client
        .post(app.url(&format!("/api/v1/agent-runs/{recoverable}/finish")))
        .header("x-csrf-token", &csrf)
        .header("idempotency-key", "admin-run-recovery-finish")
        .json(&json!({"outcome": "failed"}))
        .send()
        .await
        .unwrap();
    assert_eq!(recovered.status(), StatusCode::OK);
    let recovered: serde_json::Value = recovered.json().await.unwrap();
    assert_eq!(recovered["data"]["status"], "failed");

    let canceled = create_run(&app, &world, AgentRunBudget::default()).await;
    application::agents::cancel_run(
        &app.state.db,
        &admin,
        canceled,
        &CancelAgentRunRequest { reason: None },
    )
    .await
    .unwrap();
    let owner_credential = insert_agent_credential(&app, &world).await;
    let owner_ctx = agent_ctx(
        &world,
        owner_credential,
        Some(canceled),
        "finish-canceled-run",
    );
    let error = application::agents::finish_run(
        &app.state.db,
        &owner_ctx,
        canceled,
        &contracts::agents::FinishAgentRunRequest {
            outcome: contracts::agents::AgentRunFinishOutcome::Completed,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, contracts::ErrorCode::Conflict);
}

#[tokio::test]
async fn t11_audit_log_read_path_drops_unallowlisted_sensitive_details() {
    let (app, world) = test_app().await;
    let csrf = login_admin(&app, &world).await;
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let audit_id: Uuid = sqlx::query_scalar(
        "INSERT INTO audit_logs
            (workspace_id, actor_principal_id, action, target_type, request_id, details)
         VALUES ($1, $2, 'test.redaction', 'job', 'req-redact',
                 '{\"outcome\":\"ok\",\"password\":\"secret-value\",\"body\":\"mail body\",\"safe_but_unknown\":\"must drop\"}')
         RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(world.admin.principal_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let response = app.get("/api/v1/audit-logs?limit=200").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value = response.json().await.unwrap();
    let entry = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == audit_id.to_string())
        .expect("inserted audit entry is visible to admin");
    assert_eq!(entry["details"], json!({"outcome": "ok"}));
    assert!(entry.get("password").is_none());
    assert!(entry.get("body").is_none());
    assert!(entry.get("safe_but_unknown").is_none());
    drop(csrf);
}

#[tokio::test]
async fn t11_safe_job_retry_is_audited_without_storing_the_operator_reason() {
    let (app, world) = test_app().await;
    let csrf = login_admin(&app, &world).await;
    let job_id = insert_job(&app, world.workspace_id, "dead").await;
    let reason = "verified with provider; operator token=private-value";
    let response = app
        .client
        .post(app.url(&format!("/api/v1/jobs/{job_id}/retry-requests")))
        .header("x-csrf-token", csrf)
        .header("idempotency-key", "dead-job-safe-retry")
        .json(&json!({"reason": reason}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["data"]["state"], "queued");
    assert_eq!(body["data"]["external_effect"], true);
    assert_eq!(body["data"]["attempt_count"], 0);
    assert_eq!(
        body["data"]["result_refs"][0]["type"],
        "outbound_message_id"
    );
    assert_eq!(
        body["data"]["result_refs"][0]["id"],
        "10000000-0000-4000-8000-000000000001"
    );

    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let (state, attempts): (String, i32) =
        sqlx::query_as("SELECT state, attempt_count FROM jobs WHERE id = $1")
            .bind(job_id)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    let audit_details: serde_json::Value = sqlx::query_scalar(
        "SELECT details FROM audit_logs WHERE action = 'job.retry_requested' AND target_id = $1",
    )
    .bind(job_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let outbox_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM outbox_events WHERE event_type = 'job.retry_requested' AND aggregate_id = $1",
    )
    .bind(job_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!((state.as_str(), attempts), ("queued", 0));
    assert!(audit_details.get("reason").is_none());
    assert!(audit_details["reason_hash"].as_str().is_some());
    assert!(audit_details.to_string().find("private-value").is_none());
    assert_eq!(outbox_count, 1);
}

#[tokio::test]
async fn t11_agent_run_creation_and_cancel_use_the_same_audited_state() {
    let (app, world) = test_app().await;
    let csrf = login_admin(&app, &world).await;
    let created = app
        .client
        .post(app.url("/api/v1/agent-runs"))
        .header("x-csrf-token", &csrf)
        .header("idempotency-key", "create-agent-run-1")
        .json(&json!({
            "agent_principal_id": world.agent.principal_id,
            "objective": "Review sourced company candidates",
            "scope": {"industry": "software", "country_code": "kr"},
            "budget": {"max_tool_calls": 20, "max_proposals": 25, "max_external_searches": 5, "max_cost_usd": 2.5}
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let created: serde_json::Value = created.json().await.unwrap();
    let run_id: Uuid = created["data"]["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(created["data"]["status"], "queued");

    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    infrastructure::repo::agents::record_action(
        &mut tx,
        run_id,
        "crm_get_company",
        &[0xab; 32],
        json!({
            "company_id": "20000000-0000-4000-8000-000000000001",
            "email": "private@example.com",
            "body": "private tool content"
        }),
        None,
        true,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let detail = app.get(&format!("/api/v1/agent-runs/{run_id}")).await;
    assert_eq!(detail.status(), StatusCode::OK);
    let detail: serde_json::Value = detail.json().await.unwrap();
    assert_eq!(
        detail["data"]["actions"][0]["redacted_arguments"],
        json!({
            "company_id": "20000000-0000-4000-8000-000000000001"
        })
    );
    assert!(
        detail["data"]["actions"][0]["arguments_hash"]
            .as_str()
            .is_some()
    );

    let canceled = app
        .client
        .post(app.url(&format!("/api/v1/agent-runs/{run_id}/cancel")))
        .header("x-csrf-token", &csrf)
        .header("idempotency-key", "cancel-agent-run-1")
        .json(&json!({"reason": "request complete"}))
        .send()
        .await
        .unwrap();
    assert_eq!(canceled.status(), StatusCode::OK);
    let canceled: serde_json::Value = canceled.json().await.unwrap();
    assert_eq!(canceled["data"]["status"], "canceled");
    assert_eq!(canceled["data"]["in_flight_external_actions"], 1);

    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let (status, cancel_requested_at): (String, Option<chrono::DateTime<chrono::Utc>>) =
        sqlx::query_as("SELECT status, cancel_requested_at FROM agent_runs WHERE id = $1")
            .bind(run_id)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    let audit_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_logs WHERE action IN ('agent_run.created','agent_run.cancel_requested') AND target_id = $1",
    )
    .bind(run_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(status, "canceled");
    assert!(cancel_requested_at.is_some());
    assert_eq!(audit_count, 2);
}

#[tokio::test]
async fn t11_dashboard_reports_zero_denominator_as_not_available() {
    let (app, world) = test_app().await;
    let _csrf = login_admin(&app, &world).await;
    let response = app.get("/api/v1/dashboard/summary?scope=team").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["data"]["scope"], "team");
    assert_eq!(body["data"]["kpis"]["eligible_contacted_companies"], 0);
    assert!(body["data"]["kpis"]["reply_company_ratio"]["value"].is_null());
    assert!(body["data"]["kpis"]["meeting_conversion"]["value"].is_null());
    assert!(body["data"]["generated_at"].is_string());
}

#[tokio::test]
async fn t01_dashboard_queues_count_only_visible_mail_approvals_and_meetings() {
    let (app, world) = test_app().await;
    let hidden_mail_account = {
        let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
        sqlx::query(
            "UPDATE mail_accounts SET last_synced_at = NULL
              WHERE workspace_id = $1 AND id = $2",
        )
        .bind(world.workspace_id)
        .bind(world.mail_account_id)
        .execute(tx.conn())
        .await
        .unwrap();
        let hidden = insert_dashboard_mailbox(&mut tx, &world).await;
        insert_dashboard_thread(
            &mut tx,
            world.workspace_id,
            world.mail_account_id,
            world.member_a.principal_id,
            "open",
            "visible",
        )
        .await;
        insert_dashboard_thread(
            &mut tx,
            world.workspace_id,
            hidden,
            world.member_a.principal_id,
            "pending",
            "hidden",
        )
        .await;

        let visible_address = fixtures::insert_email_address(
            &mut tx,
            &format!("dashboard-visible-{}@example.test", Uuid::new_v4()),
        )
        .await
        .unwrap();
        let visible_outbound = fixtures::insert_outbound(
            &mut tx,
            world.mail_account_id,
            world.connection_id,
            world.member_a.principal_id,
            visible_address,
        )
        .await
        .unwrap();
        sqlx::query(
            "UPDATE outbound_messages SET status = 'unknown'
              WHERE workspace_id = $1 AND id = $2",
        )
        .bind(world.workspace_id)
        .bind(visible_outbound)
        .execute(tx.conn())
        .await
        .unwrap();

        let hidden_address = fixtures::insert_email_address(
            &mut tx,
            &format!("dashboard-hidden-{}@example.test", Uuid::new_v4()),
        )
        .await
        .unwrap();
        let hidden_outbound = fixtures::insert_outbound(
            &mut tx,
            hidden,
            world.connection_id,
            world.member_a.principal_id,
            hidden_address,
        )
        .await
        .unwrap();
        sqlx::query(
            "UPDATE outbound_messages SET status = 'unknown'
              WHERE workspace_id = $1 AND id = $2",
        )
        .bind(world.workspace_id)
        .bind(hidden_outbound)
        .execute(tx.conn())
        .await
        .unwrap();
        tx.commit().await.unwrap();
        hidden
    };

    insert_job_for(
        &app,
        world.workspace_id,
        "dead",
        "imports.validate",
        Some(world.member_a.principal_id),
    )
    .await;
    insert_job_for(
        &app,
        world.workspace_id,
        "dead",
        "imports.validate",
        Some(world.admin.principal_id),
    )
    .await;
    insert_dashboard_booking(&app, &world, world.member_a.principal_id).await;
    insert_dashboard_booking(&app, &world, world.admin.principal_id).await;
    insert_dashboard_booking_approval(&app, &world, world.member_a.principal_id).await;
    insert_dashboard_booking_approval(&app, &world, world.admin.principal_id).await;

    let _admin_csrf = login_admin(&app, &world).await;
    let team = app.get("/api/v1/dashboard/summary?scope=team").await;
    assert_eq!(team.status(), StatusCode::OK);
    let team: serde_json::Value = team.json().await.unwrap();
    assert_eq!(dashboard_queue_count(&team, "inbox_unhandled"), Some(1));
    // Requesters may decide their own approvals (migration 0044), so the
    // admin's own booking approval is actionable alongside member_a's.
    assert_eq!(dashboard_queue_count(&team, "approvals_pending"), Some(2));
    assert_eq!(dashboard_queue_count(&team, "meetings_today"), Some(2));
    assert_eq!(dashboard_queue_count(&team, "outbound_unknown"), Some(1));
    assert_eq!(dashboard_queue_count(&team, "sync_lag"), Some(1));
    assert_eq!(dashboard_queue_count(&team, "reauth_required"), Some(0));
    assert_eq!(dashboard_queue_count(&team, "dead_jobs"), Some(2));
    assert!(dashboard_queue_count(&team, "outbox_backlog").unwrap_or(0) >= 2);
    assert!(!team.to_string().contains("PRIVATE subject"));
    assert!(!team.to_string().contains("PRIVATE body"));
    assert!(!team.to_string().contains("hidden@example.test"));
    assert!(
        !team
            .to_string()
            .contains(hidden_mail_account.to_string().as_str())
    );

    let member_app = TestApp::spawn_with_state(AppState::with_fake_providers(
        app.state.db.clone(),
        Arc::new(Config::test()),
        None,
    ))
    .await;
    let member_login = member_app
        .login(
            world.member_a.email.as_deref().unwrap(),
            infrastructure::testing::TEST_PASSWORD,
        )
        .await;
    assert_eq!(member_login.status(), StatusCode::OK);
    let member = member_app.get("/api/v1/dashboard/summary?scope=me").await;
    assert_eq!(member.status(), StatusCode::OK);
    let member: serde_json::Value = member.json().await.unwrap();
    assert_eq!(dashboard_queue_count(&member, "inbox_unhandled"), Some(1));
    assert_eq!(dashboard_queue_count(&member, "meetings_today"), Some(1));
    assert_eq!(dashboard_queue_count(&member, "outbound_unknown"), Some(1));
    assert_eq!(dashboard_queue_count(&member, "sync_lag"), Some(1));
    assert_eq!(dashboard_queue_count(&member, "reauth_required"), Some(0));
    assert_eq!(dashboard_queue_count(&member, "dead_jobs"), Some(1));
    assert!(dashboard_queue_count(&member, "approvals_pending").is_none());
    assert!(dashboard_queue_count(&member, "outbox_backlog").is_none());
    assert!(!member.to_string().contains("PRIVATE subject"));
    assert!(!member.to_string().contains("PRIVATE body"));
    assert!(!member.to_string().contains("hidden@example.test"));
}
