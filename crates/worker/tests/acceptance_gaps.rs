//! Acceptance coverage gaps for approved outbound snapshots, suppression
//! enrollment pauses, and research-candidate send eligibility (T08, T11, T35).

mod common;

use application::sending;
use chrono::{Duration, Utc};
use contracts::ErrorCode;
use contracts::Scope;
use contracts::ScopeSet;
use contracts::research::{CreateLeadProposalsRequest, DecisionKind, LeadProposalDecisionRequest};
use contracts::sending::{
    CreateMessageDraftRequest, CreateOutboundApprovalRequest, FreezeMessageDraftRequest,
    SubmitOutboundRequest,
};
use infrastructure::config::Config;
use infrastructure::db::{Db, Tx};
use infrastructure::jobs::{self, JobCtx};
use infrastructure::repo::campaigns as campaigns_repo;
use infrastructure::repo::compliance_core;
use infrastructure::testing::{
    TestPrincipal, TestWorld, ctx_for, ctx_for_credential, seed_workspace,
};
use serde_json::json;
use uuid::Uuid;
use worker::{WorkerContext, registry, registry_with_context};

async fn test_db() -> Option<(Db, Db, TestWorld)> {
    let worker_db = common::test_db().await?;
    let seed_db = common::seed_db().await;
    let world = seed_workspace(&seed_db).await.unwrap();
    Some((worker_db, seed_db, world))
}

async fn enable_sending(api_db: &Db, workspace_id: Uuid) {
    let mut tx = api_db.begin(workspace_id).await.unwrap();
    sqlx::query("UPDATE workspaces SET emergency_stop = false WHERE id = $1")
        .bind(workspace_id)
        .execute(tx.conn())
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

/// Create a verified sender and an enabled channel for the requested route.
async fn sender_and_channel(tx: &mut Tx<'_>, world: &TestWorld, purpose: &str) -> (Uuid, Uuid) {
    let workspace_id = tx.workspace_id().unwrap();
    let provider = if purpose == "correspondence" {
        "zoho_mail"
    } else {
        "delivery_provider"
    };
    let connection_id = if provider == "zoho_mail" {
        world.connection_id
    } else {
        let owner_id: Uuid = sqlx::query_scalar(
            "SELECT connected_by FROM integration_connections
              WHERE workspace_id = $1 AND id = $2",
        )
        .bind(workspace_id)
        .bind(world.connection_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        sqlx::query_scalar(
            "INSERT INTO integration_connections
                (workspace_id, provider, region, secret_ref, status, connected_by)
             VALUES ($1, $2, 'us', 'acceptance-test-secret', 'active', $3)
             RETURNING id",
        )
        .bind(workspace_id)
        .bind(provider)
        .bind(owner_id)
        .fetch_one(tx.conn())
        .await
        .unwrap()
    };
    let sender_id: Uuid = sqlx::query_scalar(
        "INSERT INTO sender_identities
            (workspace_id, mail_account_id, address, display_name, status, verified_at)
         VALUES ($1, $2, $3, 'Acceptance Sender', 'verified', now())
         RETURNING id",
    )
    .bind(workspace_id)
    .bind(world.mail_account_id)
    .bind(format!("sender-{}@test.dev", Uuid::new_v4().simple()))
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let channel_id: Uuid = sqlx::query_scalar(
        "INSERT INTO delivery_channels
            (workspace_id, mail_account_id, sender_identity_id, connection_id,
             provider, purpose, name, policy_status, enabled)
         VALUES ($1, $2, $3, $4, $5, $6, 'Acceptance channel', 'allowed', true)
         RETURNING id",
    )
    .bind(workspace_id)
    .bind(world.mail_account_id)
    .bind(sender_id)
    .bind(connection_id)
    .bind(provider)
    .bind(purpose)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    (sender_id, channel_id)
}

async fn insert_address(tx: &mut Tx<'_>, address: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO email_addresses (workspace_id, address_original, normalized_key)
         VALUES ($1, $2, lower($2)) RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(address)
    .fetch_one(tx.conn())
    .await
    .unwrap()
}

async fn insert_valid_basis(
    tx: &mut Tx<'_>,
    address_id: Uuid,
    reviewer_id: Uuid,
    purpose: &str,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO communication_bases
            (workspace_id, email_address_id, purpose, jurisdiction, basis_type,
             status, evidence_ref, collected_at, reviewed_by, reviewed_at)
         VALUES ($1, $2, $3, 'KR', 'explicit_consent', 'valid', 'acceptance fixture',
                 now(), $4, now())
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(address_id)
    .bind(purpose)
    .bind(reviewer_id)
    .fetch_one(tx.conn())
    .await
    .unwrap()
}

fn draft_request(
    world: &TestWorld,
    recipient_id: Uuid,
    purpose: &str,
) -> CreateMessageDraftRequest {
    CreateMessageDraftRequest {
        mail_account_id: world.mail_account_id,
        to_email_address_id: recipient_id,
        purpose: purpose.into(),
        subject: "Acceptance snapshot subject".into(),
        body_text: "Acceptance snapshot body".into(),
        body_html: None,
        template_version_id: None,
        variables: None,
        thread_id: None,
        reply_to_message_id: None,
        expected_last_inbound_id: None,
        visibility: None,
    }
}

async fn freeze_draft(
    db: &Db,
    world: &TestWorld,
    author: &TestPrincipal,
    channel_id: Uuid,
    req: &CreateMessageDraftRequest,
) -> (Uuid, String) {
    let ctx = ctx_for(world, author, "acceptance-freeze");
    let draft = sending::create_draft(db, &ctx, req).await.unwrap();
    let frozen = sending::freeze_draft(
        db,
        &ctx,
        draft.draft_id,
        1,
        &FreezeMessageDraftRequest {
            delivery_channel_id: channel_id,
            sender_identity_id: None,
            meeting_type_id: None,
            tracking_requested: false,
        },
    )
    .await
    .unwrap();
    (frozen.outbound_message_id, frozen.payload_hash)
}

fn decode_hash(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let digit = |c: u8| match c {
                b'0'..=b'9' => c - b'0',
                b'a'..=b'f' => c - b'a' + 10,
                b'A'..=b'F' => c - b'A' + 10,
                _ => panic!("invalid hex hash"),
            };
            digit(pair[0]) * 16 + digit(pair[1])
        })
        .collect()
}

async fn approve_outbound(db: &Db, world: &TestWorld, outbound_id: Uuid, hash: &str) -> Uuid {
    let requester = ctx_for(world, &world.admin, "acceptance-approval-request");
    let request = sending::create_approval_request(
        db,
        &requester,
        outbound_id,
        &CreateOutboundApprovalRequest {
            reason: "acceptance snapshot review".into(),
            expected_payload_hash: hash.into(),
            expires_at: None,
        },
    )
    .await
    .unwrap();
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    sqlx::query(
        "INSERT INTO approval_decisions
            (workspace_id, approval_request_id, decided_by, decision,
             snapshot_hash, version_at_decision)
         VALUES ($1, $2, $3, 'approved', $4, 1)",
    )
    .bind(world.workspace_id)
    .bind(request.approval_request_id)
    .bind(world.manager.principal_id)
    .bind(decode_hash(hash))
    .execute(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE approval_requests SET status = 'approved'
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(world.workspace_id)
    .bind(request.approval_request_id)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    request.approval_request_id
}

#[tokio::test]
async fn t08_approved_outbound_body_recipient_and_sender_route_are_immutable() {
    let Some((db, api_db, world)) = test_db().await else {
        return;
    };
    let workspace_id = world.workspace_id;
    enable_sending(&api_db, workspace_id).await;

    let (
        sender_id,
        channel_id,
        other_sender_id,
        other_channel_id,
        recipient_id,
        other_recipient_id,
        other_basis_id,
    ) = {
        let mut tx = db.begin(workspace_id).await.unwrap();
        let (sender_id, channel_id) = sender_and_channel(&mut tx, &world, "marketing").await;
        let (other_sender_id, other_channel_id) =
            sender_and_channel(&mut tx, &world, "marketing").await;
        let recipient_id =
            insert_address(&mut tx, &format!("primary-{}@example.test", Uuid::new_v4())).await;
        let other_recipient_id = insert_address(
            &mut tx,
            &format!("alternate-{}@example.test", Uuid::new_v4()),
        )
        .await;
        insert_valid_basis(&mut tx, recipient_id, world.admin.principal_id, "marketing").await;
        let other_basis_id = insert_valid_basis(
            &mut tx,
            other_recipient_id,
            world.admin.principal_id,
            "marketing",
        )
        .await;
        tx.commit().await.unwrap();
        (
            sender_id,
            channel_id,
            other_sender_id,
            other_channel_id,
            recipient_id,
            other_recipient_id,
            other_basis_id,
        )
    };

    let req = draft_request(&world, recipient_id, "marketing");
    let (outbound_id, payload_hash) =
        freeze_draft(&db, &world, &world.admin, channel_id, &req).await;
    let approval_id = approve_outbound(&db, &world, outbound_id, &payload_hash).await;

    // A direct post-approval edit to each frozen dimension is rejected by
    // PostgreSQL, even when the replacement references valid workspace rows.
    let mut tx = db.begin(workspace_id).await.unwrap();
    let body_edit = sqlx::query(
        "UPDATE outbound_messages SET body_text = 'changed after approval'
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(workspace_id)
    .bind(outbound_id)
    .execute(tx.conn())
    .await;
    assert!(
        body_edit.is_err(),
        "approved outbound body edit was accepted"
    );
    tx.rollback().await.unwrap();

    let mut tx = db.begin(workspace_id).await.unwrap();
    let recipient_edit = sqlx::query(
        "UPDATE outbound_messages
            SET to_email_address_id = $3, communication_basis_id = $4,
                to_address_snapshot = 'alternate@example.test'
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(workspace_id)
    .bind(outbound_id)
    .bind(other_recipient_id)
    .bind(other_basis_id)
    .execute(tx.conn())
    .await;
    assert!(
        recipient_edit.is_err(),
        "approved recipient edit was accepted"
    );
    tx.rollback().await.unwrap();

    let mut tx = db.begin(workspace_id).await.unwrap();
    let sender_edit = sqlx::query(
        "UPDATE outbound_messages
            SET sender_identity_id = $3, delivery_channel_id = $4
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(workspace_id)
    .bind(outbound_id)
    .bind(other_sender_id)
    .bind(other_channel_id)
    .execute(tx.conn())
    .await;
    assert!(
        sender_edit.is_err(),
        "approved sender route edit was accepted"
    );
    tx.rollback().await.unwrap();

    let mut tx = db.begin(workspace_id).await.unwrap();
    let (stored_sender, stored_channel, stored_recipient, stored_payload_hash): (
        Uuid,
        Uuid,
        Uuid,
        Vec<u8>,
    ) = sqlx::query_as(
        "SELECT sender_identity_id, delivery_channel_id, to_email_address_id, payload_hash
           FROM outbound_messages WHERE workspace_id = $1 AND id = $2",
    )
    .bind(workspace_id)
    .bind(outbound_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(stored_sender, sender_id);
    assert_eq!(stored_channel, channel_id);
    assert_eq!(stored_recipient, recipient_id);
    assert_eq!(stored_payload_hash, decode_hash(&payload_hash));

    // Submit still compares the caller's hash with the exact frozen row,
    // then accepts only the approved snapshot hash.
    let ctx = ctx_for(&world, &world.admin, "acceptance-submit");
    let wrong_hash = if payload_hash == "0".repeat(64) {
        "1".repeat(64)
    } else {
        "0".repeat(64)
    };
    let mismatched = sending::submit(
        &db,
        &ctx,
        outbound_id,
        &SubmitOutboundRequest {
            expected_payload_hash: wrong_hash,
            not_before: None,
        },
        "acceptance-wrong-hash",
    )
    .await
    .unwrap_err();
    assert_eq!(mismatched.code, ErrorCode::SnapshotMismatch);

    let submitted = sending::submit(
        &db,
        &ctx,
        outbound_id,
        &SubmitOutboundRequest {
            expected_payload_hash: payload_hash.clone(),
            not_before: None,
        },
        "acceptance-correct-hash",
    )
    .await
    .unwrap()
    .body;
    assert_eq!(submitted.status, "queued");

    let mut tx = db.begin(workspace_id).await.unwrap();
    let (approval_status, approval_hash): (String, Vec<u8>) = sqlx::query_as(
        "SELECT status, snapshot_hash FROM approval_requests
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(workspace_id)
    .bind(approval_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(approval_status, "approved");
    assert_eq!(approval_hash, decode_hash(&payload_hash));
}

async fn create_pending_enrollment(db: &Db, world: &TestWorld) -> (Uuid, Uuid, Uuid) {
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let (_, channel_id) = sender_and_channel(&mut tx, world, "marketing").await;
    let template_id: Uuid = sqlx::query_scalar(
        "INSERT INTO email_templates (workspace_id, name, created_by)
         VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(format!("pause-fixture-{}", Uuid::new_v4()))
    .bind(world.admin.principal_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let template_version_id: Uuid = sqlx::query_scalar(
        "INSERT INTO template_versions
            (workspace_id, template_id, version_no, subject, body_text,
             variable_schema, content_hash, created_by)
         VALUES ($1, $2, 1, 'Subject', 'Body', '{}', decode(md5(random()::text), 'hex'), $3)
         RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(template_id)
    .bind(world.admin.principal_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let campaign_id = campaigns_repo::insert_campaign(
        &mut tx,
        world.mail_account_id,
        "Suppression pause acceptance",
        None,
        world.admin.principal_id,
    )
    .await
    .unwrap();
    let revision_id = campaigns_repo::insert_revision(
        &mut tx,
        campaign_id,
        world.mail_account_id,
        channel_id,
        1,
        "UTC",
        &json!({}),
        true,
        true,
        world.admin.principal_id,
    )
    .await
    .unwrap();
    let step_id = campaigns_repo::insert_step(&mut tx, revision_id, template_version_id, 1, 0)
        .await
        .unwrap();
    let launch_id = campaigns_repo::insert_launch(
        &mut tx,
        revision_id,
        channel_id,
        world.admin.principal_id,
        Utc::now() + Duration::days(1),
    )
    .await
    .unwrap();
    let address_id =
        insert_address(&mut tx, &format!("paused-{}@example.test", Uuid::new_v4())).await;
    let contact_id: Uuid = sqlx::query_scalar(
        "INSERT INTO contacts (workspace_id, name) VALUES ($1, 'Suppression test') RETURNING id",
    )
    .bind(world.workspace_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO contact_emails (workspace_id, contact_id, email_address_id, is_primary)
         VALUES ($1, $2, $3, true)",
    )
    .bind(world.workspace_id)
    .bind(contact_id)
    .bind(address_id)
    .execute(tx.conn())
    .await
    .unwrap();
    let enrollment_id = campaigns_repo::insert_enrollment(
        &mut tx,
        launch_id,
        revision_id,
        address_id,
        Some(contact_id),
        "pending",
        None,
    )
    .await
    .unwrap();
    let _ = step_id;
    tx.commit().await.unwrap();
    (address_id, enrollment_id, launch_id)
}

async fn run_registered_handler(db: &Db, workspace_id: Uuid, kind: &str, job_id: Uuid) {
    let handler = registry()
        .get(kind)
        .expect("expected worker handler registration");
    let mut tx = db.begin(workspace_id).await.unwrap();
    let claimed = jobs::claim(&mut tx, "acceptance-gaps", 10, &[kind])
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let job = claimed
        .into_iter()
        .find(|job| job.id == job_id)
        .expect("target job should be claimed by its registered kind");
    let ctx = JobCtx {
        job_id: job.id,
        workspace_id,
        lease_token: job.lease_token.expect("claimed job lease token"),
        attempt_count: job.attempt_count,
        lease_owner: "acceptance-gaps".into(),
    };
    let mut tx = db.begin(workspace_id).await.unwrap();
    handler.handle(&ctx, &mut tx, job.payload).await.unwrap();
    jobs::complete(&mut tx, job.id, ctx.lease_token)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn t11_committed_suppression_runs_registered_pause_enrollments_handler() {
    let Some((db, _api_db, world)) = test_db().await else {
        return;
    };
    let workspace_id = world.workspace_id;
    let (address_id, enrollment_id, _launch_id) = create_pending_enrollment(&db, &world).await;

    let mut tx = db.begin(workspace_id).await.unwrap();
    let suppression_id = compliance_core::insert_suppression(
        &mut tx,
        address_id,
        "all",
        "manual",
        "manual",
        Some(world.admin.principal_id),
        None,
    )
    .await
    .unwrap();
    let job_id: Uuid = sqlx::query_scalar(
        "SELECT id FROM jobs
          WHERE workspace_id = $1 AND kind = 'compliance.pause_enrollments'
            AND payload->>'suppression_id' = $2",
    )
    .bind(workspace_id)
    .bind(suppression_id.to_string())
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    run_registered_handler(&db, workspace_id, "compliance.pause_enrollments", job_id).await;

    let mut tx = db.begin(workspace_id).await.unwrap();
    let (status, stop_reason): (String, Option<String>) = sqlx::query_as(
        "SELECT status, stop_reason FROM campaign_enrollments
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(workspace_id)
    .bind(enrollment_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let job_state: String =
        sqlx::query_scalar("SELECT state FROM jobs WHERE workspace_id = $1 AND id = $2")
            .bind(workspace_id)
            .bind(job_id)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    let pause_audit_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_logs
          WHERE workspace_id = $1 AND action = 'campaign.enrollments_suppressed'
            AND target_id = $2",
    )
    .bind(workspace_id)
    .bind(address_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.rollback().await.unwrap();

    assert_eq!(status, "suppressed");
    assert_eq!(stop_reason.as_deref(), Some("suppressed"));
    assert_eq!(job_state, "completed");
    assert_eq!(pause_audit_count, 1);
}

fn proposed_candidate(email: &str) -> CreateLeadProposalsRequest {
    serde_json::from_value(json!({
        "proposals": [{
            "external_key": format!("acceptance-{}", Uuid::new_v4()),
            "company_name": "Candidate Company",
            "domain": format!("candidate-{}.example", Uuid::new_v4().simple()),
            "proposed_data": {
                "industry": "manufacturing",
                "contacts": [{"name": "Research Candidate", "email": email}]
            },
            "evidence": [{
                "field_path": "industry",
                "source_url": "https://public.example/candidate",
                "observed_at": "2026-01-01T00:00:00Z",
                "value": "manufacturing",
                "evidence_type": "observed"
            }]
        }]
    }))
    .unwrap()
}

async fn run_registered_dispatch(db: &Db, workspace_id: Uuid, job_id: Uuid) {
    let context = WorkerContext::from_config(db.clone(), &Config::test());
    let handler = registry_with_context(&context)
        .get("sending.dispatch_outbound")
        .expect("dispatch handler should be registered with provider context");
    let mut tx = db.begin(workspace_id).await.unwrap();
    let claimed = jobs::claim(
        &mut tx,
        "acceptance-dispatch",
        10,
        &["sending.dispatch_outbound"],
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let job = claimed
        .into_iter()
        .find(|job| job.id == job_id)
        .expect("target dispatch job should be claimed");
    let ctx = JobCtx {
        job_id: job.id,
        workspace_id,
        lease_token: job.lease_token.expect("claimed job lease token"),
        attempt_count: job.attempt_count,
        lease_owner: "acceptance-dispatch".into(),
    };
    let mut tx = db.begin(workspace_id).await.unwrap();
    handler.handle(&ctx, &mut tx, job.payload).await.unwrap();
    jobs::complete(&mut tx, job.id, ctx.lease_token)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn t35_accepted_candidate_fails_preflight_and_dispatch_send_basis_check() {
    let Some((db, api_db, world)) = test_db().await else {
        return;
    };
    let workspace_id = world.workspace_id;
    enable_sending(&api_db, workspace_id).await;

    let candidate_email = format!("candidate-{}@example.test", Uuid::new_v4().simple());
    let agent_ctx = ctx_for_credential(
        &world,
        &world.agent,
        Uuid::new_v4(),
        ScopeSet::of([Scope::ResearchPropose]),
        "acceptance-t35-proposal",
    );
    let proposal = application::research::create_proposals(
        &db,
        &agent_ctx,
        &proposed_candidate(&candidate_email),
    )
    .await
    .unwrap();
    let proposal_id = proposal.proposals[0].id;
    let manager_ctx = ctx_for(&world, &world.manager, "acceptance-t35-accept");
    let accepted = application::research::decide(
        &db,
        &manager_ctx,
        proposal_id,
        1,
        &LeadProposalDecisionRequest {
            decision: DecisionKind::Accept,
            company_id: None,
            reason: Some("candidate accepted for review".into()),
        },
    )
    .await
    .unwrap();
    let recipient_id = accepted.email_address_ids[0];

    let mut tx = db.begin(workspace_id).await.unwrap();
    let candidate_basis: (String, Option<Uuid>) = sqlx::query_as(
        "SELECT status, reviewed_by FROM communication_bases
          WHERE workspace_id = $1 AND email_address_id = $2 AND purpose = 'marketing'",
    )
    .bind(workspace_id)
    .bind(recipient_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(candidate_basis.0, "pending");
    assert!(candidate_basis.1.is_none());

    let channel_id = {
        let mut tx = db.begin(workspace_id).await.unwrap();
        let (_, channel_id) = sender_and_channel(&mut tx, &world, "marketing").await;
        tx.commit().await.unwrap();
        channel_id
    };
    let request = draft_request(&world, recipient_id, "marketing");
    let (outbound_id, payload_hash) =
        freeze_draft(&db, &world, &world.admin, channel_id, &request).await;
    let ctx = ctx_for(&world, &world.admin, "acceptance-t35-preflight");

    let preflight = sending::preflight(&db, &ctx, outbound_id).await.unwrap();
    let basis_check = preflight
        .checks
        .iter()
        .find(|check| check.name == "send_basis")
        .expect("preflight must report the send-basis predicate");
    assert!(
        !basis_check.passed,
        "pending candidate basis became eligible"
    );
    assert_eq!(basis_check.code.as_deref(), Some("SEND_BASIS_UNVERIFIED"));

    // Approval can queue the outbound; the worker's dispatch handler must
    // still re-evaluate the real basis before crossing the provider boundary.
    approve_outbound(&db, &world, outbound_id, &payload_hash).await;
    let submitted = sending::submit(
        &db,
        &ctx,
        outbound_id,
        &SubmitOutboundRequest {
            expected_payload_hash: payload_hash,
            not_before: None,
        },
        "acceptance-t35-submit",
    )
    .await
    .unwrap()
    .body;
    assert_eq!(submitted.status, "queued");

    run_registered_dispatch(&db, workspace_id, submitted.job_id).await;

    let mut tx = db.begin(workspace_id).await.unwrap();
    let (status, failure_code): (String, Option<String>) = sqlx::query_as(
        "SELECT status, failure_code FROM outbound_messages
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(workspace_id)
    .bind(outbound_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let attempt_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM send_attempts
          WHERE workspace_id = $1 AND outbound_message_id = $2",
    )
    .bind(workspace_id)
    .bind(outbound_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.rollback().await.unwrap();

    assert_eq!(status, "canceled");
    assert_eq!(failure_code.as_deref(), Some("SEND_BASIS_UNVERIFIED"));
    assert_eq!(attempt_count, 0, "dispatch must stop before a send attempt");
}
