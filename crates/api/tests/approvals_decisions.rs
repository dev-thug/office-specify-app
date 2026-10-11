//! approvals module HTTP tests (spec §5.5, B.3.9, A.6.3): decision
//! validation (pending/expiry/If-Match/snapshot), self-approval + agent +
//! member bans, mailbox can_approve grants, single-decision uniqueness,
//! revoke cancelling queued side effects, and channel/emergency re-checks.

use api::AppState;
use api::testing::TestApp;
use infrastructure::config::Config;
use infrastructure::db::{Db, ROLE_API};
use infrastructure::repo::approvals::{self, ApprovalTarget};
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
    // workspaces start in emergency stop (fail-closed default).
    {
        let mut tx = db.begin(world.workspace_id).await.unwrap();
        sqlx::query("UPDATE workspaces SET emergency_stop = false WHERE id = $1")
            .bind(world.workspace_id)
            .execute(tx.conn())
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    let db = db.with_default_workspace(Some(world.workspace_id));
    let app = TestApp::spawn_with_state(AppState::with_fake_providers(
        db,
        Arc::new(Config::test()),
        None,
    ))
    .await;
    Some((app, world))
}

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

// --------------------------------------------------------------- fixtures

/// Enabled + policy-allowed marketing channel → outbound draft that a
/// `message_submit` approval request can pin.
async fn outbound_draft(db: &Db, world: &TestWorld) -> (Uuid /*outbound*/, [u8; 32]) {
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
         VALUES ($1, $2, $3, 'Mk', 'verified', now()) RETURNING id",
    )
    .bind(ws)
    .bind(world.mail_account_id)
    .bind(format!("mk-{}@sender.dev", Uuid::new_v4().simple()))
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
    let addr: Uuid = sqlx::query_scalar(
        "INSERT INTO email_addresses
            (workspace_id, address_original, normalized_key, kind, verification_status)
         VALUES ($1, $2, $2, 'person', 'verified') RETURNING id",
    )
    .bind(ws)
    .bind(format!("rcpt-{}@rcpt.dev", Uuid::new_v4().simple()))
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let (outbound, hash): (Uuid, Vec<u8>) = sqlx::query_as(
        "INSERT INTO outbound_messages
            (workspace_id, mail_account_id, delivery_channel_id, sender_identity_id,
             to_email_address_id, requested_by, purpose,
             from_address_snapshot, to_address_snapshot, subject, body_text, payload_hash)
         VALUES ($1, $2, $3, $4, $5, $6, 'marketing',
                 'mk@sender.dev', 'rcpt@rcpt.dev', 'subj', 'body',
                 sha256(random()::text::bytea))
         RETURNING id, payload_hash",
    )
    .bind(ws)
    .bind(world.mail_account_id)
    .bind(channel)
    .bind(ident)
    .bind(addr)
    .bind(world.admin.principal_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let mut h = [0u8; 32];
    h.copy_from_slice(&hash);
    (outbound, h)
}

/// Insert a pending `message_submit` request pinned to the outbound's
/// current payload hash; returns the request id.
async fn message_request(
    db: &Db,
    world: &TestWorld,
    requested_by: Uuid,
    outbound: Uuid,
    hash: &[u8; 32],
    expires_in: chrono::Duration,
) -> Uuid {
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let id = approvals::insert_request(
        &mut tx,
        requested_by,
        ApprovalTarget::OutboundMessage(outbound),
        hash,
        chrono::Utc::now() + expires_in,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    id
}

fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

async fn scalar<
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

// ------------------------------------------------------------------ tests

/// Approval records permission to execute, but does not queue delivery.
#[tokio::test]
async fn approve_message_submit_leaves_outbound_unqueued() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (manager, csrf) = login(&app, world.manager.email.as_deref().unwrap()).await;
    let db = &app.state.db;
    let (outbound, hash) = outbound_draft(db, &world).await;
    let request = message_request(
        db,
        &world,
        world.admin.principal_id,
        outbound,
        &hash,
        chrono::Duration::days(1),
    )
    .await;

    let res = post(
        &app,
        &manager,
        &csrf,
        &format!("/api/v1/approvals/{request}/decisions"),
        json!({"decision": "approved", "expected_snapshot_hash": hex(&hash)}),
        Some("ap-1"),
        Some(1),
    )
    .await;
    let data = TestApp::data(res).await;
    assert_eq!(data["approval_status"], "approved");
    assert_eq!(data["execution_status"], "ready");
    assert!(data["job_id"].is_null());
    assert_eq!(data["provider_accepted_count"], 0);

    let status: String = scalar(
        db,
        world.workspace_id,
        "SELECT status FROM outbound_messages WHERE workspace_id=$1 AND id=$2",
        outbound,
    )
    .await;
    assert_eq!(status, "draft");

    // Per-message message_submit approval remains the standalone send gate.
    let preflight = manager
        .post(app.url(&format!("/api/v1/outbound-messages/{outbound}/preflight")))
        .header("x-csrf-token", &csrf)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(preflight.status(), StatusCode::OK);
    let preflight = TestApp::data(preflight).await;
    for name in ["approval_state", "approval_hash", "approver_permission"] {
        let check = preflight["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["name"] == name)
            .unwrap();
        assert_eq!(check["passed"], true, "standalone {name} should still pass");
    }

    let jobs: i64 = scalar(
        db,
        world.workspace_id,
        "SELECT count(*) FROM jobs WHERE workspace_id=$1 AND kind='sending.dispatch_outbound' AND state='queued'",
        world.workspace_id,
    )
    .await;
    assert_eq!(jobs, 0);
    // audit + outbox in the same tx.
    let audits: i64 = scalar(
        db,
        world.workspace_id,
        "SELECT count(*) FROM audit_logs WHERE workspace_id=$1 AND action='approval.decided' AND target_id=$2",
        request,
    )
    .await;
    assert_eq!(audits, 1);
    let events: i64 = scalar(
        db,
        world.workspace_id,
        "SELECT count(*) FROM outbox_events WHERE workspace_id=$1 AND event_type='approval.decided' AND aggregate_id=$2",
        request,
    )
    .await;
    assert_eq!(events, 1);
}

/// Rejection cancels the unsent draft; no dispatch job.
#[tokio::test]
async fn reject_message_submit_cancels_outbound() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (manager, csrf) = login(&app, world.manager.email.as_deref().unwrap()).await;
    let db = &app.state.db;
    let (outbound, hash) = outbound_draft(db, &world).await;
    let request = message_request(
        db,
        &world,
        world.admin.principal_id,
        outbound,
        &hash,
        chrono::Duration::days(1),
    )
    .await;

    let res = post(
        &app,
        &manager,
        &csrf,
        &format!("/api/v1/approvals/{request}/decisions"),
        json!({"decision": "rejected", "expected_snapshot_hash": hex(&hash), "reason": "off brand"}),
        Some("rj-1"),
        Some(1),
    )
    .await;
    let data = TestApp::data(res).await;
    assert_eq!(data["approval_status"], "rejected");
    assert_eq!(data["execution_status"], "none");
    let status: String = scalar(
        db,
        world.workspace_id,
        "SELECT status FROM outbound_messages WHERE workspace_id=$1 AND id=$2",
        outbound,
    )
    .await;
    assert_eq!(status, "canceled");
}

/// Owners with approval permission may review their own requests.
#[tokio::test]
async fn requester_with_approval_permission_can_approve_without_sending() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (admin, csrf) = login(&app, world.admin.email.as_deref().unwrap()).await;
    let db = &app.state.db;
    let (outbound, hash) = outbound_draft(db, &world).await;
    let request = message_request(
        db,
        &world,
        world.admin.principal_id,
        outbound,
        &hash,
        chrono::Duration::days(1),
    )
    .await;
    let res = post(
        &app,
        &admin,
        &csrf,
        &format!("/api/v1/approvals/{request}/decisions"),
        json!({"decision":"approved","expected_snapshot_hash":hex(&hash)}),
        Some("self-1"),
        Some(1),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let result: Value = res.json().await.unwrap();
    assert_eq!(result["data"]["approval_status"], "approved");
    assert!(result["data"]["job_id"].is_null());
    let outbound_response = admin
        .get(app.url(&format!("/api/v1/outbound-messages/{outbound}")))
        .send()
        .await
        .unwrap();
    assert_eq!(outbound_response.status(), StatusCode::OK);
    let outbound_data = TestApp::data(outbound_response).await;
    assert_eq!(outbound_data["approval_valid"], true);
}

/// T08 — a decision pinning a hash that does not match the request's
/// frozen snapshot is rejected; the approval cannot be used.
#[tokio::test]
async fn t08_snapshot_mismatch_blocks_decision() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (manager, csrf) = login(&app, world.manager.email.as_deref().unwrap()).await;
    let db = &app.state.db;
    let (outbound, _hash) = outbound_draft(db, &world).await;
    // Request pins a hash that is NOT the outbound's current payload hash —
    // equivalent to the target having changed after the request was made
    // (the frozen-snapshot trigger makes the real mutation impossible, so
    // the mismatch models T08's post-approval-change case).
    let wrong = [0xEEu8; 32];
    let request = message_request(
        db,
        &world,
        world.admin.principal_id,
        outbound,
        &wrong,
        chrono::Duration::days(1),
    )
    .await;

    let res = post(
        &app,
        &manager,
        &csrf,
        &format!("/api/v1/approvals/{request}/decisions"),
        json!({"decision": "approved", "expected_snapshot_hash": hex(&wrong)}),
        Some("sm-1"),
        Some(1),
    )
    .await;
    let p = TestApp::problem(res, StatusCode::CONFLICT).await;
    assert_eq!(p["code"], "SNAPSHOT_MISMATCH");
}

#[tokio::test]
async fn expired_request_is_rejected() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (manager, csrf) = login(&app, world.manager.email.as_deref().unwrap()).await;
    let db = &app.state.db;
    let (outbound, hash) = outbound_draft(db, &world).await;
    let request = message_request(
        db,
        &world,
        world.admin.principal_id,
        outbound,
        &hash,
        chrono::Duration::seconds(-60),
    )
    .await;

    let res = post(
        &app,
        &manager,
        &csrf,
        &format!("/api/v1/approvals/{request}/decisions"),
        json!({"decision": "approved", "expected_snapshot_hash": hex(&hash)}),
        Some("exp-1"),
        Some(1),
    )
    .await;
    let p = TestApp::problem(res, StatusCode::CONFLICT).await;
    assert_eq!(p["code"], "APPROVAL_EXPIRED");
}

/// Exactly one decision per request — the second decide conflicts (the
/// UNIQUE on approval_decisions.approval_request_id is the anchor; the
/// pending check fires earlier).
#[tokio::test]
async fn second_decision_conflicts() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (manager, csrf) = login(&app, world.manager.email.as_deref().unwrap()).await;
    let db = &app.state.db;
    let (outbound, hash) = outbound_draft(db, &world).await;
    let request = message_request(
        db,
        &world,
        world.admin.principal_id,
        outbound,
        &hash,
        chrono::Duration::days(1),
    )
    .await;

    for (i, key) in ["d1", "d2"].iter().enumerate() {
        let res = post(
            &app,
            &manager,
            &csrf,
            &format!("/api/v1/approvals/{request}/decisions"),
            json!({"decision": "approved", "expected_snapshot_hash": hex(&hash)}),
            Some(key),
            Some(1 + i as i64), // version bumps after the first decision
        )
        .await;
        if i == 0 {
            assert_eq!(res.status(), StatusCode::OK);
        } else {
            let p = TestApp::problem(res, StatusCode::CONFLICT).await;
            assert_eq!(p["code"], "CONFLICT", "second decision must conflict: {p}");
        }
    }
}

/// If-Match is required and enforced on decisions.
#[tokio::test]
async fn decision_requires_matching_if_match() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (manager, csrf) = login(&app, world.manager.email.as_deref().unwrap()).await;
    let db = &app.state.db;
    let (outbound, hash) = outbound_draft(db, &world).await;
    let request = message_request(
        db,
        &world,
        world.admin.principal_id,
        outbound,
        &hash,
        chrono::Duration::days(1),
    )
    .await;

    // Missing If-Match → 428.
    let res = post(
        &app,
        &manager,
        &csrf,
        &format!("/api/v1/approvals/{request}/decisions"),
        json!({"decision": "approved", "expected_snapshot_hash": hex(&hash)}),
        Some("ifm-1"),
        None,
    )
    .await;
    let p = TestApp::problem(res, StatusCode::PRECONDITION_REQUIRED).await;
    assert_eq!(p["code"], "VERSION_PRECONDITION_REQUIRED");

    // Stale → 412.
    let res = post(
        &app,
        &manager,
        &csrf,
        &format!("/api/v1/approvals/{request}/decisions"),
        json!({"decision": "approved", "expected_snapshot_hash": hex(&hash)}),
        Some("ifm-2"),
        Some(42),
    )
    .await;
    let p = TestApp::problem(res, StatusCode::PRECONDITION_FAILED).await;
    assert_eq!(p["code"], "VERSION_MISMATCH");
}

/// A member (role) cannot decide even with a can_approve grant — the role
/// check precedes the mailbox grant (§5.3 approval is admin/manager only).
#[tokio::test]
async fn member_cannot_decide() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (member_a, csrf) = login(&app, world.member_a.email.as_deref().unwrap()).await;
    let db = &app.state.db;
    let (outbound, hash) = outbound_draft(db, &world).await;
    let request = message_request(
        db,
        &world,
        world.admin.principal_id,
        outbound,
        &hash,
        chrono::Duration::days(1),
    )
    .await;

    let res = post(
        &app,
        &member_a,
        &csrf,
        &format!("/api/v1/approvals/{request}/decisions"),
        json!({"decision": "approved", "expected_snapshot_hash": hex(&hash)}),
        Some("mem-1"),
        Some(1),
    )
    .await;
    let p = TestApp::problem(res, StatusCode::FORBIDDEN).await;
    assert_eq!(p["code"], "FORBIDDEN");
}

/// Agents may never approve (§5.3) — exercised at the application layer
/// with a service-credential ctx since agents authenticate out-of-band.
#[tokio::test]
async fn agent_cannot_decide() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let db = &app.state.db;
    let (outbound, hash) = outbound_draft(db, &world).await;
    let request = message_request(
        db,
        &world,
        world.admin.principal_id,
        outbound,
        &hash,
        chrono::Duration::days(1),
    )
    .await;

    let ctx = infrastructure::testing::ctx_for_credential(
        &world,
        &world.agent,
        Uuid::new_v4(),
        contracts::ScopeSet::of([contracts::Scope::ApprovalsRead]),
        "req-agent-decide",
    );
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let err = application::approvals::decide(
        &mut tx,
        &ctx,
        request,
        1,
        &contracts::approvals::CreateDecisionRequest {
            decision: "approved".to_string(),
            expected_snapshot_hash: hex(&hash),
            reason: None,
        },
    )
    .await
    .expect_err("agents must not decide approvals");
    assert_eq!(err.code, contracts::ErrorCode::Forbidden);
    tx.rollback().await.unwrap();
}

/// Revoking a pending request settles it (revoked); a later decide fails.
#[tokio::test]
async fn revoke_pending_request() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (admin, csrf) = login(&app, world.admin.email.as_deref().unwrap()).await;
    let db = &app.state.db;
    let (outbound, hash) = outbound_draft(db, &world).await;
    let request = message_request(
        db,
        &world,
        world.admin.principal_id,
        outbound,
        &hash,
        chrono::Duration::days(1),
    )
    .await;

    let res = post(
        &app,
        &admin,
        &csrf,
        &format!("/api/v1/approvals/{request}/revoke"),
        json!({"reason": "no longer needed"}),
        None,
        Some(1),
    )
    .await;
    let data = TestApp::data(res).await;
    assert_eq!(data["status"], "revoked");

    // Deciding a revoked request conflicts.
    let (manager, mcsrf) = login(&app, world.manager.email.as_deref().unwrap()).await;
    let res = post(
        &app,
        &manager,
        &mcsrf,
        &format!("/api/v1/approvals/{request}/decisions"),
        json!({"decision": "approved", "expected_snapshot_hash": hex(&hash)}),
        Some("rv-1"),
        Some(data["version"].as_i64().unwrap()),
    )
    .await;
    TestApp::problem(res, StatusCode::CONFLICT).await;
}

/// Revoking an approved request before its job ran cancels the queued
/// dispatch and the outbound (A.6.3 — external effects not yet executed
/// must not execute).
#[tokio::test]
async fn revoke_approved_cancels_queued_dispatch() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (admin, acsrf) = login(&app, world.admin.email.as_deref().unwrap()).await;
    let (manager, mcsrf) = login(&app, world.manager.email.as_deref().unwrap()).await;
    let db = &app.state.db;
    let (outbound, hash) = outbound_draft(db, &world).await;
    let request = message_request(
        db,
        &world,
        world.admin.principal_id,
        outbound,
        &hash,
        chrono::Duration::days(1),
    )
    .await;
    let res = post(
        &app,
        &manager,
        &mcsrf,
        &format!("/api/v1/approvals/{request}/decisions"),
        json!({"decision": "approved", "expected_snapshot_hash": hex(&hash)}),
        Some("rv2-dec"),
        Some(1),
    )
    .await;
    let data = TestApp::data(res).await;
    let v = data["version"].as_i64().unwrap();

    // Approval is deliberately separate from execution. The human explicitly
    // submits after reviewing the approved snapshot; revocation can still
    // cancel that queued job before the provider boundary.
    let submitted = post(
        &app,
        &admin,
        &acsrf,
        &format!("/api/v1/outbound-messages/{outbound}/submit"),
        json!({"expected_payload_hash": hex(&hash)}),
        Some("revoke-approved-human-submit"),
        None,
    )
    .await;
    let submitted = TestApp::data(submitted).await;
    assert_eq!(submitted["status"], "queued");

    let res = post(
        &app,
        &admin,
        &acsrf,
        &format!("/api/v1/approvals/{request}/revoke"),
        json!({"reason": "changed my mind"}),
        None,
        Some(v),
    )
    .await;
    let data = TestApp::data(res).await;
    assert_eq!(data["status"], "revoked");

    let outbound_status: String = scalar(
        db,
        world.workspace_id,
        "SELECT status FROM outbound_messages WHERE workspace_id=$1 AND id=$2",
        outbound,
    )
    .await;
    assert_eq!(
        outbound_status, "canceled",
        "queued outbound must be canceled"
    );
    let live_jobs: i64 = scalar(
        db,
        world.workspace_id,
        "SELECT count(*) FROM jobs WHERE workspace_id=$1 AND kind='sending.dispatch_outbound' AND state='queued'",
        world.workspace_id,
    )
    .await;
    assert_eq!(live_jobs, 0, "the queued dispatch job must be canceled");
}

/// An approval that has not been explicitly submitted can be revoked while
/// the immutable outbound remains a draft; subsequent submit must fail.
#[tokio::test]
async fn revoke_approved_before_submit_leaves_draft_and_blocks_submit() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (admin, admin_csrf) = login(&app, world.admin.email.as_deref().unwrap()).await;
    let (manager, manager_csrf) = login(&app, world.manager.email.as_deref().unwrap()).await;
    let db = &app.state.db;
    let (outbound, hash) = outbound_draft(db, &world).await;
    let request = message_request(
        db,
        &world,
        world.admin.principal_id,
        outbound,
        &hash,
        chrono::Duration::days(1),
    )
    .await;
    let approved = TestApp::data(
        post(
            &app,
            &manager,
            &manager_csrf,
            &format!("/api/v1/approvals/{request}/decisions"),
            json!({"decision":"approved","expected_snapshot_hash":hex(&hash)}),
            Some("revoke-before-submit-decision"),
            Some(1),
        )
        .await,
    )
    .await;
    assert_eq!(approved["execution_status"], "ready");

    let revoked = TestApp::data(
        post(
            &app,
            &admin,
            &admin_csrf,
            &format!("/api/v1/approvals/{request}/revoke"),
            json!({"reason":"changed my mind"}),
            None,
            Some(approved["version"].as_i64().unwrap()),
        )
        .await,
    )
    .await;
    assert_eq!(revoked["status"], "revoked");

    let status_response = manager
        .get(app.url(&format!("/api/v1/outbound-messages/{outbound}")))
        .send()
        .await
        .unwrap();
    assert_eq!(status_response.status(), StatusCode::OK);
    let status = TestApp::data(status_response).await;
    assert_eq!(status["status"], "draft");
    assert_eq!(status["approval_status"], "revoked");
    assert_eq!(status["approval_valid"], false);

    let submit = post(
        &app,
        &admin,
        &admin_csrf,
        &format!("/api/v1/outbound-messages/{outbound}/submit"),
        json!({"expected_payload_hash":hex(&hash)}),
        Some("submit-after-approval-revoked"),
        None,
    )
    .await;
    let problem = TestApp::problem(submit, StatusCode::CONFLICT).await;
    assert_eq!(problem["code"], "CONFLICT");
}

#[tokio::test]
async fn approved_retired_provider_message_approval_is_read_only_history() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (admin, acsrf) = login(&app, world.admin.email.as_deref().unwrap()).await;
    let (manager, mcsrf) = login(&app, world.manager.email.as_deref().unwrap()).await;
    let db = &app.state.db;
    let (outbound, hash) = outbound_draft(db, &world).await;
    let request = message_request(
        db,
        &world,
        world.admin.principal_id,
        outbound,
        &hash,
        chrono::Duration::days(1),
    )
    .await;
    let decision = post(
        &app,
        &manager,
        &mcsrf,
        &format!("/api/v1/approvals/{request}/decisions"),
        json!({"decision":"approved","expected_snapshot_hash":hex(&hash)}),
        Some("retired-message-approve"),
        Some(1),
    )
    .await;
    assert_eq!(decision.status(), StatusCode::OK);

    let owner = Db::connect(
        &std::env::var("DATABASE_URL").expect("DATABASE_URL must be set"),
        None,
    )
    .await
    .unwrap();
    let mut tx = owner.begin(world.workspace_id).await.unwrap();
    let channel_id: Uuid = sqlx::query_scalar(
        "SELECT delivery_channel_id FROM outbound_messages
          WHERE workspace_id=$1 AND id=$2",
    )
    .bind(world.workspace_id)
    .bind(outbound)
    .fetch_one(tx.conn())
    .await
    .unwrap();
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
    .bind(channel_id)
    .bind(retired_connection)
    .execute(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE outbound_messages
            SET status='canceled', failure_code='PROVIDER_RETIRED', version=version+1
          WHERE workspace_id=$1 AND id=$2",
    )
    .bind(world.workspace_id)
    .bind(outbound)
    .execute(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE jobs
            SET state='canceled', last_error_code='PROVIDER_RETIRED', finished_at=now()
          WHERE workspace_id=$1 AND kind='sending.dispatch_outbound'
            AND payload->>'outbound_message_id'=$2",
    )
    .bind(world.workspace_id)
    .bind(outbound.to_string())
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let detail_response = admin
        .get(app.url(&format!("/api/v1/approvals/{request}")))
        .send()
        .await
        .unwrap();
    assert_eq!(detail_response.status(), StatusCode::OK);
    let detail = TestApp::data(detail_response).await;
    assert_eq!(detail["status"], "approved");
    assert_eq!(detail["retired_provider"], true);
    assert_eq!(detail["can_decide"], false);

    let version: i64 = scalar(
        db,
        world.workspace_id,
        "SELECT version FROM approval_requests WHERE workspace_id=$1 AND id=$2",
        request,
    )
    .await;
    let revoke = post(
        &app,
        &admin,
        &acsrf,
        &format!("/api/v1/approvals/{request}/revoke"),
        json!({"reason":"legacy provider retirement"}),
        Some("retired-message-revoke"),
        Some(version),
    )
    .await;
    let problem = TestApp::problem(revoke, StatusCode::CONFLICT).await;
    assert_eq!(problem["code"], "CONFLICT");
    assert_eq!(
        scalar::<String>(
            db,
            world.workspace_id,
            "SELECT status FROM approval_requests WHERE workspace_id=$1 AND id=$2",
            request,
        )
        .await,
        "approved"
    );
}

/// Channel policy is re-verified at decision time (A.6.3 "채널…재검사"):
/// disabling the channel between request and decision blocks approval.
#[tokio::test]
async fn disabled_channel_blocks_decision() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (manager, csrf) = login(&app, world.manager.email.as_deref().unwrap()).await;
    let db = &app.state.db;
    let (outbound, hash) = outbound_draft(db, &world).await;
    let request = message_request(
        db,
        &world,
        world.admin.principal_id,
        outbound,
        &hash,
        chrono::Duration::days(1),
    )
    .await;

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    sqlx::query(
        "UPDATE delivery_channels SET enabled = false
          WHERE workspace_id = $1 AND id = (SELECT delivery_channel_id FROM outbound_messages
                                           WHERE workspace_id=$1 AND id=$2)",
    )
    .bind(world.workspace_id)
    .bind(outbound)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let res = post(
        &app,
        &manager,
        &csrf,
        &format!("/api/v1/approvals/{request}/decisions"),
        json!({"decision": "approved", "expected_snapshot_hash": hex(&hash)}),
        Some("ch-1"),
        Some(1),
    )
    .await;
    let p = TestApp::problem(res, StatusCode::UNPROCESSABLE_ENTITY).await;
    assert_eq!(p["code"], "PROVIDER_POLICY_UNVERIFIED");
}

/// List visibility: the requester sees their own request; an unrelated
/// member gets 404 on get and an empty list.
#[tokio::test]
async fn list_scopes_to_requester_and_approvers() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (admin, _) = login(&app, world.admin.email.as_deref().unwrap()).await;
    let (member_b, _) = login(&app, world.member_b.email.as_deref().unwrap()).await;
    let db = &app.state.db;
    let (outbound, hash) = outbound_draft(db, &world).await;
    let request = message_request(
        db,
        &world,
        world.admin.principal_id,
        outbound,
        &hash,
        chrono::Duration::days(1),
    )
    .await;

    // Requester sees it in the queue.
    let res = admin
        .get(app.url("/api/v1/approvals?status=pending"))
        .send()
        .await
        .unwrap();
    let data = TestApp::data(res).await;
    assert!(
        data.as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == request.to_string())
    );

    // member_b has no mailbox grant and isn't the requester → 404 on get.
    let res = member_b
        .get(app.url(&format!("/api/v1/approvals/{request}")))
        .send()
        .await
        .unwrap();
    TestApp::problem(res, StatusCode::NOT_FOUND).await;
    let res = member_b
        .get(app.url("/api/v1/approvals"))
        .send()
        .await
        .unwrap();
    let data = TestApp::data(res).await;
    assert_eq!(data.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn approval_detail_hides_mail_preview_without_mail_read_scope() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let db = &app.state.db;
    let (outbound, hash) = outbound_draft(db, &world).await;
    let request = message_request(
        db,
        &world,
        world.admin.principal_id,
        outbound,
        &hash,
        chrono::Duration::days(1),
    )
    .await;
    let ctx = infrastructure::testing::ctx_for_credential(
        &world,
        &world.manager,
        Uuid::new_v4(),
        contracts::ScopeSet::of([
            contracts::Scope::ApprovalsRead,
            contracts::Scope::MailCompose,
        ]),
        "approval-detail-without-mail-read",
    );

    let detail = application::approvals::get(db, &ctx, request)
        .await
        .unwrap();
    assert!(
        detail.target.preview.is_none(),
        "mail.read scope is required before approval detail includes recipient/content"
    );
    assert!(detail.snapshot.is_none());
    assert!(!detail.can_decide);
}
