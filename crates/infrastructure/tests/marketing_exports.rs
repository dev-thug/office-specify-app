//! Database contract tests for durable native Zoho Campaigns exports.

mod common;

use chrono::{Duration, Utc};
use contracts::ErrorCode;
use domain::marketing::{MarketingExportState, OperationClaim, OperationResolution};
use infrastructure::{
    db::{Db, ROLE_API, ROLE_REPORT, ROLE_WORKER, Tx},
    repo::{
        approvals::{self, ApprovalTarget},
        campaigns, marketing_exports as repo,
    },
    testing::{TestWorld, seed_workspace},
};
use serde_json::json;
use uuid::Uuid;

struct Fixture {
    launch_id: Uuid,
    revision_id: Uuid,
    step_id: Uuid,
    delivery_channel_id: Uuid,
    sender_identity_id: Uuid,
    email_address_id: Uuid,
    normalized_address: String,
    outbound_message_id: Uuid,
}

async fn insert_template_version(tx: &mut Tx<'_>, created_by: Uuid) -> Uuid {
    let template: Uuid = sqlx::query_scalar(
        "INSERT INTO email_templates (workspace_id, name, created_by)
         VALUES ($1, 'marketing export fixture', $2) RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(created_by)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    sqlx::query_scalar(
        "INSERT INTO template_versions
            (workspace_id, template_id, version_no, subject, body_text, variable_schema,
             content_hash, created_by)
         VALUES ($1, $2, 1, 'Subject', 'Body', '{}', decode(repeat('11', 32), 'hex'), $3)
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(template)
    .bind(created_by)
    .fetch_one(tx.conn())
    .await
    .unwrap()
}

/// A valid building launch, one step, one pending recipient and its source
/// outbound. Supplying an address lets another launch bind the same address
/// to a different outbound for the cross-launch guard test.
async fn fixture(db: &Db, world: &TestWorld, address: Option<Uuid>) -> Fixture {
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let ws = world.workspace_id;
    let sender_identity_id: Uuid = sqlx::query_scalar(
        "INSERT INTO sender_identities
            (workspace_id, mail_account_id, address, display_name, status, verified_at)
         VALUES ($1, $2, $3, 'Marketing Sender', 'verified', now()) RETURNING id",
    )
    .bind(ws)
    .bind(world.mail_account_id)
    .bind(format!("sender+{}@test.dev", Uuid::new_v4()))
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let delivery_channel_id: Uuid = sqlx::query_scalar(
        "INSERT INTO delivery_channels
            (workspace_id, mail_account_id, sender_identity_id, connection_id,
             provider, purpose, name)
         VALUES ($1, $2, $3, $4, 'zoho_mail', 'marketing', 'Zoho Campaigns')
         RETURNING id",
    )
    .bind(ws)
    .bind(world.mail_account_id)
    .bind(sender_identity_id)
    .bind(world.connection_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let template_version_id = insert_template_version(&mut tx, world.admin.principal_id).await;
    let campaign_id = campaigns::insert_campaign(
        &mut tx,
        world.mail_account_id,
        "Zoho export fixture",
        None,
        world.admin.principal_id,
    )
    .await
    .unwrap();
    let revision_id = campaigns::insert_revision(
        &mut tx,
        campaign_id,
        world.mail_account_id,
        delivery_channel_id,
        1,
        "UTC",
        &json!({}),
        true,
        true,
        world.admin.principal_id,
    )
    .await
    .unwrap();
    let step_id = campaigns::insert_step(&mut tx, revision_id, template_version_id, 1, 0)
        .await
        .unwrap();
    let launch_id = campaigns::insert_launch(
        &mut tx,
        revision_id,
        delivery_channel_id,
        world.admin.principal_id,
        Utc::now() + Duration::days(1),
    )
    .await
    .unwrap();
    let (email_address_id, normalized_address) = match address {
        Some(id) => {
            let normalized: String = sqlx::query_scalar(
                "SELECT normalized_key FROM email_addresses
                  WHERE workspace_id = $1 AND id = $2",
            )
            .bind(ws)
            .bind(id)
            .fetch_one(tx.conn())
            .await
            .unwrap();
            (id, normalized)
        }
        None => {
            let original = format!("recipient+{}@example.test", Uuid::new_v4());
            let normalized = original.to_ascii_lowercase();
            let id: Uuid = sqlx::query_scalar(
            "INSERT INTO email_addresses
                (workspace_id, address_original, normalized_key, kind, verification_status, verified_at)
             VALUES ($1, $2, lower($2), 'person', 'verified', now()) RETURNING id",
            )
            .bind(ws)
            .bind(original)
            .fetch_one(tx.conn())
            .await
            .unwrap();
            (id, normalized)
        }
    };
    let enrollment_id = campaigns::insert_enrollment(
        &mut tx,
        launch_id,
        revision_id,
        email_address_id,
        None,
        "pending",
        None,
    )
    .await
    .unwrap();
    let outbound_message_id = campaigns::insert_step_outbound(
        &mut tx,
        world.mail_account_id,
        delivery_channel_id,
        sender_identity_id,
        email_address_id,
        None,
        launch_id,
        revision_id,
        enrollment_id,
        step_id,
        world.admin.principal_id,
        "sender@example.test",
        "recipient@example.test",
        "Source subject",
        "Source body",
        Some("<p>Source body</p>"),
        &[0x31; 32],
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    Fixture {
        launch_id,
        revision_id,
        step_id,
        delivery_channel_id,
        sender_identity_id,
        email_address_id,
        normalized_address,
        outbound_message_id,
    }
}

fn new_export(world: &TestWorld, f: &Fixture) -> repo::NewMarketingExport {
    repo::NewMarketingExport {
        launch_id: f.launch_id,
        revision_id: f.revision_id,
        step_id: f.step_id,
        delivery_channel_id: f.delivery_channel_id,
        connection_id: world.connection_id,
        source_manifest_hash: vec![0x41; 32],
        content_hash: vec![0x42; 32],
        audience_hash: vec![0x43; 32],
        export_hash: vec![0x44; 32],
        subject: "Exported subject".to_string(),
        body_text: "Exported text".to_string(),
        body_html: "<p>Exported HTML</p>".to_string(),
        sender_address: "sender@example.test".to_string(),
        sender_name: "Marketing Sender".to_string(),
        field_map: json!({"meeting.booking_url": "BookingUrl"}),
        prepared_by: world.admin.principal_id,
        expires_at: Utc::now() + Duration::days(1),
    }
}

async fn insert_ready_export(db: &Db, world: &TestWorld, f: &Fixture) -> repo::MarketingExportRow {
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let source_manifest_hash = [0x41; 32];
    assert_eq!(
        campaigns::freeze_launch_row(&mut tx, f.launch_id, &source_manifest_hash)
            .await
            .unwrap(),
        1
    );
    let export = repo::insert_export(&mut tx, new_export(world, f))
        .await
        .unwrap();
    assert!(
        repo::update_state(
            &mut tx,
            export.id,
            export.version,
            MarketingExportState::Syncing,
        )
        .await
        .unwrap()
    );
    let syncing = repo::get_export(&mut tx, export.id).await.unwrap().unwrap();
    assert!(
        repo::update_state(
            &mut tx,
            export.id,
            syncing.version,
            MarketingExportState::Ready,
        )
        .await
        .unwrap()
    );
    let ready_export = repo::get_export(&mut tx, export.id).await.unwrap().unwrap();
    tx.commit().await.unwrap();
    ready_export
}

async fn ready_export_with_approved_launch_request(
    db: &Db,
    world: &TestWorld,
    f: &Fixture,
) -> (repo::MarketingExportRow, Uuid, Uuid) {
    let ready_export = insert_ready_export(db, world, f).await;
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let source_manifest_hash = [0x41; 32];
    let request_id = approvals::insert_request(
        &mut tx,
        world.admin.principal_id,
        ApprovalTarget::CampaignLaunch(f.launch_id),
        &source_manifest_hash,
        Utc::now() + Duration::days(1),
    )
    .await
    .unwrap();
    let approver_id = world.manager.principal_id;
    approvals::insert_decision(
        &mut tx,
        request_id,
        approver_id,
        "approved",
        &source_manifest_hash,
        None,
        1,
    )
    .await
    .unwrap();
    assert_eq!(
        approvals::set_request_status(&mut tx, request_id, "approved", 1)
            .await
            .unwrap(),
        1
    );
    tx.commit().await.unwrap();
    (ready_export, request_id, approver_id)
}

fn recipient(f: &Fixture, normalized_address: &str) -> repo::NewExportRecipient {
    repo::NewExportRecipient {
        email_address_id: f.email_address_id,
        outbound_message_id: f.outbound_message_id,
        normalized_address: normalized_address.to_string(),
        variables: json!({"meeting.booking_url": "https://booking.example.test/opaque"}),
        sync_status: "pending".to_string(),
    }
}

fn assert_conflict(err: &contracts::AppError, message: &str) {
    assert_eq!(err.code, ErrorCode::Conflict, "{message}: {err}");
}

fn assert_validation(err: &contracts::AppError, message: &str) {
    assert_eq!(err.code, ErrorCode::ValidationFailed, "{message}: {err}");
}

#[tokio::test]
async fn exports_are_unique_versioned_and_snapshot_columns_are_immutable() {
    let db = common::test_db().await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let f = fixture(&db, &world, None).await;

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let export = repo::insert_export(&mut tx, new_export(&world, &f))
        .await
        .unwrap();
    assert_eq!(export.state, "preparing");
    assert_eq!(
        repo::get_export(&mut tx, export.id)
            .await
            .unwrap()
            .unwrap()
            .id,
        export.id
    );
    assert_eq!(
        repo::get_export_for_update(&mut tx, export.id)
            .await
            .unwrap()
            .unwrap()
            .id,
        export.id
    );
    assert_eq!(
        repo::get_export_by_launch(&mut tx, f.launch_id)
            .await
            .unwrap()
            .unwrap()
            .id,
        export.id
    );
    assert_eq!(repo::list_exports(&mut tx, 10).await.unwrap().len(), 1);

    sqlx::query("SAVEPOINT duplicate_marketing_export")
        .execute(tx.conn())
        .await
        .unwrap();
    let duplicate = repo::insert_export(&mut tx, new_export(&world, &f))
        .await
        .expect_err("a launch can have only one durable export");
    sqlx::query("ROLLBACK TO SAVEPOINT duplicate_marketing_export")
        .execute(tx.conn())
        .await
        .unwrap();
    assert_conflict(&duplicate, "one export per launch");

    repo::update_execution_metadata(
        &mut tx,
        export.id,
        repo::ExecutionMetadata {
            provider_list_key: Some("list-1".into()),
            topic_id: Some("topic-1".into()),
            campaign_key: Some("campaign-1".into()),
            last_checked_at: Some(Utc::now()),
            report: Some(json!({"active": 1})),
            recipient_count: Some(1),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    repo::update_execution_metadata(&mut tx, export.id, repo::ExecutionMetadata::default())
        .await
        .unwrap();
    let with_metadata = repo::get_export(&mut tx, export.id).await.unwrap().unwrap();
    assert_eq!(with_metadata.provider_list_key.as_deref(), Some("list-1"));
    assert_eq!(with_metadata.topic_id.as_deref(), Some("topic-1"));
    assert_eq!(with_metadata.campaign_key.as_deref(), Some("campaign-1"));
    assert_eq!(with_metadata.report, json!({"active": 1}));
    assert_eq!(with_metadata.recipient_count, 1);

    assert!(
        repo::update_state(
            &mut tx,
            export.id,
            with_metadata.version,
            MarketingExportState::Syncing,
        )
        .await
        .unwrap()
    );
    assert!(
        !repo::update_state(
            &mut tx,
            export.id,
            with_metadata.version,
            MarketingExportState::Ready,
        )
        .await
        .unwrap(),
        "stale expected versions must not update the export"
    );

    sqlx::query("SAVEPOINT immutable_export_snapshot")
        .execute(tx.conn())
        .await
        .unwrap();
    let mutation = sqlx::query(
        "UPDATE marketing_exports SET body_text = 'rewritten'
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(world.workspace_id)
    .bind(export.id)
    .execute(tx.conn())
    .await;
    sqlx::query("ROLLBACK TO SAVEPOINT immutable_export_snapshot")
        .execute(tx.conn())
        .await
        .unwrap();
    let error = mutation.expect_err("export content snapshots must be immutable");
    assert!(error.to_string().contains("immutable"), "{error}");
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn launch_values_and_export_recipients_are_immutable_and_launch_bound() {
    let db = common::test_db().await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let f = fixture(&db, &world, None).await;
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let export = repo::insert_export(&mut tx, new_export(&world, &f))
        .await
        .unwrap();

    let values = [repo::NewLaunchValue {
        email_address_id: f.email_address_id,
        variables: json!({"contact.first_name": "Taylor"}),
    }];
    repo::record_launch_values(&mut tx, f.launch_id, &values)
        .await
        .unwrap();
    let saved_values = repo::list_launch_values(&mut tx, f.launch_id)
        .await
        .unwrap();
    assert_eq!(saved_values.len(), 1);
    assert_eq!(saved_values[0].variables, values[0].variables);
    sqlx::query("SAVEPOINT duplicate_launch_value")
        .execute(tx.conn())
        .await
        .unwrap();
    let duplicate_value = repo::record_launch_values(&mut tx, f.launch_id, &values)
        .await
        .expect_err("launch values are append-only and cannot be replaced");
    sqlx::query("ROLLBACK TO SAVEPOINT duplicate_launch_value")
        .execute(tx.conn())
        .await
        .unwrap();
    assert_conflict(&duplicate_value, "duplicate launch variable snapshot");

    sqlx::query("SAVEPOINT immutable_launch_values")
        .execute(tx.conn())
        .await
        .unwrap();
    let mutation = sqlx::query(
        "UPDATE marketing_launch_values SET variables = '{}'::jsonb
          WHERE workspace_id = $1 AND launch_id = $2 AND email_address_id = $3",
    )
    .bind(world.workspace_id)
    .bind(f.launch_id)
    .bind(f.email_address_id)
    .execute(tx.conn())
    .await;
    sqlx::query("ROLLBACK TO SAVEPOINT immutable_launch_values")
        .execute(tx.conn())
        .await
        .unwrap();
    assert!(
        mutation
            .expect_err("launch variable values are immutable")
            .to_string()
            .contains("immutable")
    );

    // The launch/address/outbound triplet is valid and ready to store.
    repo::replace_recipients(&mut tx, export.id, &[recipient(&f, &f.normalized_address)])
        .await
        .unwrap();
    let recipients = repo::list_recipients(&mut tx, export.id).await.unwrap();
    assert_eq!(recipients.len(), 1);
    assert_eq!(recipients[0].outbound_message_id, f.outbound_message_id);
    assert_eq!(
        recipients[0].variables,
        recipient(&f, &f.normalized_address).variables
    );
    assert!(
        repo::set_recipient_status(&mut tx, export.id, f.email_address_id, "active",)
            .await
            .unwrap()
    );
    assert_eq!(
        repo::list_recipients(&mut tx, export.id).await.unwrap()[0].sync_status,
        "active"
    );

    // Two different Office address IDs cannot claim a provider address that
    // does not exactly match their stored normalized key.
    let second_address: Uuid = sqlx::query_scalar(
        "INSERT INTO email_addresses
            (workspace_id, address_original, normalized_key, kind, verification_status, verified_at)
         VALUES ($1, $2, lower($2), 'person', 'verified', now()) RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(format!("second+{}@example.test", Uuid::new_v4()))
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let second_enrollment = campaigns::insert_enrollment(
        &mut tx,
        f.launch_id,
        f.revision_id,
        second_address,
        None,
        "pending",
        None,
    )
    .await
    .unwrap();
    let second_outbound = campaigns::insert_step_outbound(
        &mut tx,
        world.mail_account_id,
        f.delivery_channel_id,
        f.sender_identity_id,
        second_address,
        None,
        f.launch_id,
        f.revision_id,
        second_enrollment,
        f.step_id,
        world.admin.principal_id,
        "sender@example.test",
        "second@example.test",
        "Source subject",
        "Source body",
        None,
        &[0x33; 32],
    )
    .await
    .unwrap();
    sqlx::query("SAVEPOINT duplicate_export_recipient_address")
        .execute(tx.conn())
        .await
        .unwrap();
    let malformed_address = repo::replace_recipients(
        &mut tx,
        export.id,
        &[
            recipient(&f, &f.normalized_address),
            repo::NewExportRecipient {
                email_address_id: second_address,
                outbound_message_id: second_outbound,
                normalized_address: f.normalized_address.clone(),
                variables: json!({}),
                sync_status: "pending".to_string(),
            },
        ],
    )
    .await
    .expect_err("a recipient cannot claim another address's normalized key");
    sqlx::query("ROLLBACK TO SAVEPOINT duplicate_export_recipient_address")
        .execute(tx.conn())
        .await
        .unwrap();
    assert_validation(
        &malformed_address,
        "provider address must match normalized Office address",
    );

    // A second launch can use the same Office address and step definition,
    // but its outbound is not a valid source for this export.
    let other_launch = campaigns::insert_launch(
        &mut tx,
        f.revision_id,
        f.delivery_channel_id,
        world.admin.principal_id,
        Utc::now() + Duration::days(1),
    )
    .await
    .unwrap();
    let other_enrollment = campaigns::insert_enrollment(
        &mut tx,
        other_launch,
        f.revision_id,
        f.email_address_id,
        None,
        "pending",
        None,
    )
    .await
    .unwrap();
    let other_outbound = campaigns::insert_step_outbound(
        &mut tx,
        world.mail_account_id,
        f.delivery_channel_id,
        f.sender_identity_id,
        f.email_address_id,
        None,
        other_launch,
        f.revision_id,
        other_enrollment,
        f.step_id,
        world.admin.principal_id,
        "sender@example.test",
        "recipient@example.test",
        "Other launch",
        "Other content",
        None,
        &[0x32; 32],
    )
    .await
    .unwrap();
    let wrong_source = repo::NewExportRecipient {
        outbound_message_id: other_outbound,
        ..recipient(&f, &f.normalized_address)
    };
    sqlx::query("SAVEPOINT wrong_source_outbound")
        .execute(tx.conn())
        .await
        .unwrap();
    let wrong_launch = repo::replace_recipients(&mut tx, export.id, &[wrong_source])
        .await
        .expect_err("recipient source outbound must belong to the same launch");
    sqlx::query("ROLLBACK TO SAVEPOINT wrong_source_outbound")
        .execute(tx.conn())
        .await
        .unwrap();
    assert_conflict(&wrong_launch, "source outbound launch binding");

    assert_eq!(
        campaigns::freeze_launch_row(&mut tx, f.launch_id, &[0x41; 32])
            .await
            .unwrap(),
        1
    );
    sqlx::query("SAVEPOINT frozen_launch_value_insert")
        .execute(tx.conn())
        .await
        .unwrap();
    let late_value = repo::record_launch_values(
        &mut tx,
        f.launch_id,
        &[repo::NewLaunchValue {
            email_address_id: second_address,
            variables: json!({"contact.first_name": "Too late"}),
        }],
    )
    .await
    .expect_err("launch variables cannot be appended after freeze");
    sqlx::query("ROLLBACK TO SAVEPOINT frozen_launch_value_insert")
        .execute(tx.conn())
        .await
        .unwrap();
    assert_validation(&late_value, "launch variables require a building launch");
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn operation_claims_are_idempotent_and_unknown_or_rejected_are_terminal() {
    let db = common::test_db().await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let f = fixture(&db, &world, None).await;
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let export = repo::insert_export(&mut tx, new_export(&world, &f))
        .await
        .unwrap();

    assert_eq!(
        repo::claim_operation(&mut tx, export.id, "send_campaign", "send-1", &[0x51; 32])
            .await
            .unwrap(),
        OperationClaim::New
    );
    assert_eq!(
        repo::claim_operation(&mut tx, export.id, "send_campaign", "send-1", &[0x51; 32])
            .await
            .unwrap(),
        OperationClaim::InProgress
    );
    sqlx::query("SAVEPOINT mismatch_operation_hash")
        .execute(tx.conn())
        .await
        .unwrap();
    let mismatch =
        repo::claim_operation(&mut tx, export.id, "send_campaign", "send-1", &[0x52; 32])
            .await
            .expect_err("a correlation key cannot be reused for a different request");
    sqlx::query("ROLLBACK TO SAVEPOINT mismatch_operation_hash")
        .execute(tx.conn())
        .await
        .unwrap();
    assert_conflict(&mismatch, "operation request hash mismatch");
    assert!(
        repo::mark_operation_unknown(
            &mut tx,
            export.id,
            "send_campaign",
            "send-1",
            Some("timeout")
        )
        .await
        .unwrap()
    );
    assert_eq!(
        repo::claim_operation(&mut tx, export.id, "send_campaign", "send-1", &[0x51; 32])
            .await
            .unwrap(),
        OperationClaim::Unknown
    );
    assert!(
        repo::reconcile_operation(
            &mut tx,
            export.id,
            "send_campaign",
            "send-1",
            Some("provider-campaign-1"),
            OperationResolution::Completed,
            None,
        )
        .await
        .unwrap()
    );
    assert_eq!(
        repo::claim_operation(&mut tx, export.id, "send_campaign", "send-1", &[0x51; 32])
            .await
            .unwrap(),
        OperationClaim::Completed {
            provider_id: Some("provider-campaign-1".to_string()),
        }
    );

    assert_eq!(
        repo::claim_operation(
            &mut tx,
            export.id,
            "campaign_create",
            "create-1",
            &[0x61; 32]
        )
        .await
        .unwrap(),
        OperationClaim::New
    );
    assert!(
        repo::complete_operation(
            &mut tx,
            export.id,
            "campaign_create",
            "create-1",
            Some("provider-campaign-1"),
        )
        .await
        .unwrap()
    );
    assert_eq!(
        repo::claim_operation(
            &mut tx,
            export.id,
            "campaign_create",
            "create-1",
            &[0x61; 32]
        )
        .await
        .unwrap(),
        OperationClaim::Completed {
            provider_id: Some("provider-campaign-1".to_string()),
        }
    );
    assert!(
        repo::complete_operation(&mut tx, export.id, "campaign_create", "create-1", None,)
            .await
            .unwrap(),
        "a replay without an ID preserves the previously bound provider ID"
    );
    assert_eq!(
        repo::get_operation(&mut tx, export.id, "campaign_create", "create-1")
            .await
            .unwrap()
            .unwrap()
            .provider_id
            .as_deref(),
        Some("provider-campaign-1")
    );

    assert_eq!(
        repo::claim_operation(&mut tx, export.id, "contact_sync", "sync-1", &[0x71; 32])
            .await
            .unwrap(),
        OperationClaim::New
    );
    assert!(
        repo::mark_operation_rejected(
            &mut tx,
            export.id,
            "contact_sync",
            "sync-1",
            "invalid_field",
        )
        .await
        .unwrap()
    );
    assert_eq!(
        repo::claim_operation(&mut tx, export.id, "contact_sync", "sync-1", &[0x71; 32])
            .await
            .unwrap(),
        OperationClaim::Rejected {
            error_code: Some("invalid_field".to_string()),
        }
    );

    assert_eq!(
        repo::claim_operation(&mut tx, export.id, "list_create", "list-1", &[0x81; 32])
            .await
            .unwrap(),
        OperationClaim::New
    );
    assert!(
        repo::mark_operation_unknown(&mut tx, export.id, "list_create", "list-1", Some("timeout"),)
            .await
            .unwrap()
    );
    assert!(
        repo::reconcile_operation(
            &mut tx,
            export.id,
            "list_create",
            "list-1",
            None,
            OperationResolution::Rejected,
            Some("invalid_list"),
        )
        .await
        .unwrap()
    );
    assert_eq!(
        repo::claim_operation(&mut tx, export.id, "list_create", "list-1", &[0x81; 32])
            .await
            .unwrap(),
        OperationClaim::Rejected {
            error_code: Some("invalid_list".to_string()),
        }
    );
    assert_eq!(
        repo::list_operations(&mut tx, export.id)
            .await
            .unwrap()
            .len(),
        4
    );
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn export_review_requires_matching_approval_and_is_append_only() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL must target the dedicated schema DB");
    let db = common::test_db().await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let f = fixture(&db, &world, None).await;
    let (export, approval_request_id, approver_id) =
        ready_export_with_approved_launch_request(&db, &world, &f).await;

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    sqlx::query("SAVEPOINT review_wrong_hash")
        .execute(tx.conn())
        .await
        .unwrap();
    let wrong_hash = repo::record_export_review(
        &mut tx,
        export.id,
        approval_request_id,
        approver_id,
        &[0x91; 32],
    )
    .await
    .expect_err("review must pin the exact provider-bound export hash");
    sqlx::query("ROLLBACK TO SAVEPOINT review_wrong_hash")
        .execute(tx.conn())
        .await
        .unwrap();
    assert_validation(&wrong_hash, "review export hash mismatch");

    sqlx::query("SAVEPOINT review_wrong_actor")
        .execute(tx.conn())
        .await
        .unwrap();
    let wrong_actor = repo::record_export_review(
        &mut tx,
        export.id,
        approval_request_id,
        world.admin.principal_id,
        &export.export_hash,
    )
    .await
    .expect_err("requester and preparer cannot be recorded as the approver");
    sqlx::query("ROLLBACK TO SAVEPOINT review_wrong_actor")
        .execute(tx.conn())
        .await
        .unwrap();
    assert_validation(&wrong_actor, "review actor separation");

    repo::record_export_review(
        &mut tx,
        export.id,
        approval_request_id,
        approver_id,
        &export.export_hash,
    )
    .await
    .unwrap();
    let saved = repo::get_export_review(&mut tx, export.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.workspace_id, world.workspace_id);
    assert_eq!(saved.export_id, export.id);
    assert_eq!(saved.approval_request_id, approval_request_id);
    assert_eq!(saved.approver_id, approver_id);
    assert_eq!(saved.export_hash, export.export_hash);
    tx.commit().await.unwrap();

    // The API may create and read a binding; workers may only read it, and
    // report_reader gets no access to the approval evidence.
    for (role, can_select, can_insert) in [
        (ROLE_API, true, true),
        (ROLE_WORKER, true, false),
        (ROLE_REPORT, false, false),
    ] {
        let scoped_db = Db::connect(&url, Some(role)).await.unwrap();
        let mut tx = scoped_db.begin(world.workspace_id).await.unwrap();
        let forced_rls: (bool, bool) = sqlx::query_as(
            "SELECT relrowsecurity, relforcerowsecurity FROM pg_class
              WHERE oid = to_regclass('public.marketing_export_reviews')",
        )
        .fetch_one(tx.conn())
        .await
        .unwrap();
        assert_eq!(forced_rls, (true, true));
        let grants: (bool, bool, bool, bool) = sqlx::query_as(
            "SELECT has_table_privilege(current_user, 'public.marketing_export_reviews', 'SELECT'),
                    has_table_privilege(current_user, 'public.marketing_export_reviews', 'INSERT'),
                    has_table_privilege(current_user, 'public.marketing_export_reviews', 'UPDATE'),
                    has_table_privilege(current_user, 'public.marketing_export_reviews', 'DELETE')",
        )
        .fetch_one(tx.conn())
        .await
        .unwrap();
        assert_eq!(
            grants,
            (can_select, can_insert, false, false),
            "grants for {role}"
        );

        if can_select {
            let found: Option<Uuid> = sqlx::query_scalar(
                "SELECT export_id FROM marketing_export_reviews WHERE export_id = $1",
            )
            .bind(export.id)
            .fetch_optional(tx.conn())
            .await
            .unwrap();
            assert_eq!(
                found,
                Some(export.id),
                "{role} should read the review binding"
            );
            let mut isolated = scoped_db.begin(Uuid::new_v4()).await.unwrap();
            let hidden: Option<Uuid> = sqlx::query_scalar(
                "SELECT export_id FROM marketing_export_reviews WHERE export_id = $1",
            )
            .bind(export.id)
            .fetch_optional(isolated.conn())
            .await
            .unwrap();
            assert_eq!(
                hidden, None,
                "workspace RLS must hide review evidence from {role}"
            );
            isolated.commit().await.unwrap();
        } else {
            let denied = sqlx::query("SELECT export_hash FROM marketing_export_reviews")
                .fetch_all(tx.conn())
                .await
                .expect_err("report_reader must not read export review evidence");
            assert!(denied.to_string().contains("permission denied"), "{denied}");
            tx.rollback().await.unwrap();
            continue;
        }
        tx.commit().await.unwrap();
    }

    // Run the append-only guard as the schema owner because runtime roles
    // intentionally lack UPDATE and DELETE grants.
    let owner = Db::connect(&url, None).await.unwrap();
    let mut owner_tx = owner.begin(world.workspace_id).await.unwrap();
    sqlx::query("SAVEPOINT review_update_is_forbidden")
        .execute(owner_tx.conn())
        .await
        .unwrap();
    let update = sqlx::query(
        "UPDATE marketing_export_reviews SET export_hash = decode(repeat('00', 32), 'hex')
          WHERE workspace_id = $1 AND export_id = $2",
    )
    .bind(world.workspace_id)
    .bind(export.id)
    .execute(owner_tx.conn())
    .await;
    sqlx::query("ROLLBACK TO SAVEPOINT review_update_is_forbidden")
        .execute(owner_tx.conn())
        .await
        .unwrap();
    assert!(
        update
            .expect_err("review bindings cannot be updated")
            .to_string()
            .contains("append-only")
    );

    sqlx::query("SAVEPOINT review_delete_is_forbidden")
        .execute(owner_tx.conn())
        .await
        .unwrap();
    let delete = sqlx::query(
        "DELETE FROM marketing_export_reviews WHERE workspace_id = $1 AND export_id = $2",
    )
    .bind(world.workspace_id)
    .bind(export.id)
    .execute(owner_tx.conn())
    .await;
    sqlx::query("ROLLBACK TO SAVEPOINT review_delete_is_forbidden")
        .execute(owner_tx.conn())
        .await
        .unwrap();
    assert!(
        delete
            .expect_err("review bindings cannot be deleted")
            .to_string()
            .contains("append-only")
    );
    owner_tx.rollback().await.unwrap();
}

#[tokio::test]
async fn export_review_rejects_approved_status_without_a_decision_row() {
    let db = common::test_db().await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let f = fixture(&db, &world, None).await;
    let export = insert_ready_export(&db, &world, &f).await;
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let source_manifest_hash = [0x41_u8; 32];
    let fake_request_id: Uuid = sqlx::query_scalar(
        "INSERT INTO approval_requests
            (workspace_id, requested_by, launch_id, action_type, snapshot_hash, status, expires_at)
         VALUES ($1, $2, $3, 'campaign_launch', $4, 'approved', now() + interval '1 day')
         RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(world.admin.principal_id)
    .bind(f.launch_id)
    .bind(&source_manifest_hash[..])
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let missing_decision = repo::record_export_review(
        &mut tx,
        export.id,
        fake_request_id,
        world.manager.principal_id,
        &export.export_hash,
    )
    .await
    .expect_err("approved status without a matching approval_decision is not valid evidence");
    assert_validation(
        &missing_decision,
        "review must bind a recorded approval decision",
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn workspace_rls_and_role_grants_keep_export_contents_out_of_reports() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL must target the dedicated schema DB");
    let api_db = common::test_db().await.unwrap();
    let world = seed_workspace(&api_db).await.unwrap();
    let other = seed_workspace(&api_db).await.unwrap();
    let f = fixture(&api_db, &world, None).await;
    let export_id = {
        let mut tx = api_db.begin(world.workspace_id).await.unwrap();
        let export = repo::insert_export(&mut tx, new_export(&world, &f))
            .await
            .unwrap();
        let id = export.id;
        tx.commit().await.unwrap();
        id
    };

    let mut other_tx = api_db.begin(other.workspace_id).await.unwrap();
    let cross_workspace_value = repo::record_launch_values(
        &mut other_tx,
        f.launch_id,
        &[repo::NewLaunchValue {
            email_address_id: f.email_address_id,
            variables: json!({"contact.first_name": "Wrong workspace"}),
        }],
    )
    .await
    .expect_err("a workspace cannot bind another workspace's launch/address pair");
    assert_conflict(
        &cross_workspace_value,
        "workspace-composite launch value foreign key",
    );
    other_tx.rollback().await.unwrap();

    for table in [
        "marketing_launch_values",
        "marketing_exports",
        "marketing_export_recipients",
        "marketing_operations",
    ] {
        let mut owner_tx = api_db.begin(world.workspace_id).await.unwrap();
        let rls: (bool, bool) = sqlx::query_as(
            "SELECT relrowsecurity, relforcerowsecurity FROM pg_class
              WHERE oid = to_regclass($1)",
        )
        .bind(format!("public.{table}"))
        .fetch_one(owner_tx.conn())
        .await
        .unwrap();
        assert_eq!(rls, (true, true), "RLS must be forced on {table}");
        owner_tx.commit().await.unwrap();

        for (role, expected) in [(ROLE_API, true), (ROLE_WORKER, true), (ROLE_REPORT, false)] {
            let scoped_db = Db::connect(&url, Some(role)).await.unwrap();
            let mut tx = scoped_db.begin(world.workspace_id).await.unwrap();
            let allowed: bool = sqlx::query_scalar(
                "SELECT has_table_privilege(current_user, $1, 'SELECT')
                     AND has_table_privilege(current_user, $1, 'INSERT')
                     AND has_table_privilege(current_user, $1, 'UPDATE')
                     AND has_table_privilege(current_user, $1, 'DELETE') = $2",
            )
            .bind(format!("public.{table}"))
            .bind(table == "marketing_export_recipients")
            .fetch_one(tx.conn())
            .await
            .unwrap();
            assert_eq!(allowed, expected, "DML privileges for {role} on {table}");

            if role != ROLE_REPORT {
                let visible: Option<Uuid> =
                    sqlx::query_scalar("SELECT id FROM marketing_exports WHERE id = $1")
                        .bind(export_id)
                        .fetch_optional(tx.conn())
                        .await
                        .unwrap();
                assert_eq!(visible, Some(export_id), "{role} should read its workspace");
            } else {
                let denied = sqlx::query("SELECT subject FROM marketing_exports")
                    .fetch_all(tx.conn())
                    .await
                    .expect_err("report_reader must not read export snapshots");
                assert!(denied.to_string().contains("permission denied"), "{denied}");
                tx.rollback().await.unwrap();
                continue;
            }

            let mut isolated = scoped_db.begin(other.workspace_id).await.unwrap();
            let hidden: Option<Uuid> =
                sqlx::query_scalar("SELECT id FROM marketing_exports WHERE id = $1")
                    .bind(export_id)
                    .fetch_optional(isolated.conn())
                    .await
                    .unwrap();
            assert_eq!(hidden, None, "workspace RLS must hide {table} from {role}");
            isolated.commit().await.unwrap();
            tx.commit().await.unwrap();
        }
    }
}

#[tokio::test]
async fn aggregate_status_read_does_not_establish_unsubscribe_freshness() {
    let db = common::test_db().await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let f = fixture(&db, &world, None).await;
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let export = repo::insert_export(&mut tx, new_export(&world, &f))
        .await
        .unwrap();
    repo::replace_recipients(&mut tx, export.id, &[recipient(&f, &f.normalized_address)])
        .await
        .unwrap();
    repo::update_execution_metadata(&mut tx,export.id,repo::ExecutionMetadata {
        provider_list_key:Some("old-list".into()),last_checked_at:Some(Utc::now()),
        report:Some(json!({"provider_status":"sent","recipient_reports":{"list_unsubscribers":{"partial":true,"error_code":"unavailable"}}})),
        ..Default::default()
    }).await.unwrap();
    let cutoff = Utc::now() - Duration::minutes(5);
    let stale = infrastructure::repo::marketing::prior_provider_lists(
        &mut tx,
        world.connection_id,
        &[f.email_address_id],
        Uuid::new_v4(),
        cutoff,
        101,
    )
    .await
    .unwrap();
    assert_eq!(
        stale.len(),
        1,
        "fresh aggregate metadata must not hide unavailable unsubscribe coverage"
    );
    infrastructure::repo::marketing::mark_unsubscribe_checked(&mut tx, export.id)
        .await
        .unwrap();
    let fresh = infrastructure::repo::marketing::prior_provider_lists(
        &mut tx,
        world.connection_id,
        &[f.email_address_id],
        Uuid::new_v4(),
        cutoff,
        101,
    )
    .await
    .unwrap();
    assert!(fresh.is_empty());
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn read_only_report_refresh_preserves_only_the_pending_send_scan_checkpoint() {
    let db = common::test_db().await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let f = fixture(&db, &world, None).await;
    let export = insert_ready_export(&db, &world, &f).await;
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    repo::update_execution_metadata(&mut tx, export.id, repo::ExecutionMetadata {
        report: Some(json!({"send_scan_epoch_ms": 1234, "send_scan_more": true, "old_report": "discard"})),
        ..Default::default()
    }).await.unwrap();
    repo::update_execution_metadata(
        &mut tx,
        export.id,
        repo::ExecutionMetadata {
            report: Some(json!({"provider_status": "draft"})),
            preserve_send_scan: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let refreshed = repo::get_export(&mut tx, export.id).await.unwrap().unwrap();
    assert_eq!(
        refreshed.report,
        json!({"provider_status": "draft", "send_scan_epoch_ms": 1234, "send_scan_more": true})
    );
    repo::update_execution_metadata(
        &mut tx,
        export.id,
        repo::ExecutionMetadata {
            report: Some(json!({"send_scan_more": false})),
            preserve_send_scan: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let completed = repo::get_export(&mut tx, export.id).await.unwrap().unwrap();
    assert_eq!(completed.report["send_scan_more"], false);
    repo::update_execution_metadata(
        &mut tx,
        export.id,
        repo::ExecutionMetadata {
            report: Some(json!({"readiness": "needs_review"})),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let reset = repo::get_export(&mut tx, export.id).await.unwrap().unwrap();
    assert!(reset.report.get("send_scan_epoch_ms").is_none());
    tx.rollback().await.unwrap();
}
