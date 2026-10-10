//! Retired provider history remains eligible for local privacy redaction.
//! Unresolved native operations stay unknown and are never represented as
//! provider success; untrusted retirement metadata keeps the old fail-closed
//! guard in place.

mod common;

use async_trait::async_trait;
use chrono::{Duration, Utc};
use contracts::{AppError, ErrorCode};
use infrastructure::{
    db::{Db, ROLE_RETENTION, Tx},
    repo::{campaigns, compliance::test_fixtures as compliance_fixtures},
    retention::{CacheInvalidator, ErasureRequest, erase_contact},
    storage::LocalObjectStore,
    testing::{TestWorld, seed_workspace},
};
use serde_json::{Value, json};
use uuid::Uuid;

struct NoopCache;

#[async_trait]
impl CacheInvalidator for NoopCache {
    async fn invalidate_contact(
        &self,
        _workspace_id: Uuid,
        _contact_id: Uuid,
        _email_address_ids: &[Uuid],
    ) -> Result<(), AppError> {
        Ok(())
    }
}

async fn retention_db() -> Db {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must target an isolated test DB");
    Db::connect(&url, Some(ROLE_RETENTION))
        .await
        .expect("retention role connection failed")
}

#[allow(clippy::too_many_arguments)] // Explicit fixture dimensions keep retirement cases readable.
async fn insert_historical_export(
    tx: &mut Tx<'_>,
    world: &TestWorld,
    connection_id: Uuid,
    channel_id: Uuid,
    sender_identity_id: Uuid,
    revision_id: Uuid,
    step_id: Uuid,
    address_id: Uuid,
    contact_id: Uuid,
    trusted_retirement_report: bool,
) -> Result<(Uuid, Uuid, Uuid, Uuid), AppError> {
    let launch_id = campaigns::insert_launch(
        tx,
        revision_id,
        channel_id,
        world.admin.principal_id,
        Utc::now() + Duration::days(2),
    )
    .await?;
    let enrollment_id = campaigns::insert_enrollment(
        tx,
        launch_id,
        revision_id,
        address_id,
        Some(contact_id),
        "pending",
        None,
    )
    .await?;
    let outbound_id = campaigns::insert_step_outbound(
        tx,
        world.mail_account_id,
        channel_id,
        sender_identity_id,
        address_id,
        None,
        launch_id,
        revision_id,
        enrollment_id,
        step_id,
        world.admin.principal_id,
        "sender@example.test",
        &format!("history-{}@recipient.example.test", Uuid::new_v4()),
        "Private historical subject",
        "Private historical body",
        Some("<p>Private historical body</p>"),
        &[0x27; 32],
    )
    .await?;

    let export_report = if trusted_retirement_report {
        json!({
            "provider_retired": true,
            "provider_cancel_not_requested": true
        })
    } else {
        json!({"provider_cancel_not_requested": true})
    };
    let export_id: Uuid = sqlx::query_scalar(
        "INSERT INTO marketing_exports
            (workspace_id, launch_id, revision_id, step_id, delivery_channel_id,
             connection_id, source_manifest_hash, content_hash, audience_hash,
             export_hash, subject, body_text, body_html, sender_address,
             sender_name, field_map, state, prepared_by, expires_at, report,
             recipient_count)
         VALUES ($1, $2, $3, $4, $5, $6,
                 decode(repeat('11', 32), 'hex'),
                 decode(repeat('22', 32), 'hex'),
                 decode(repeat('33', 32), 'hex'),
                 decode(repeat('44', 32), 'hex'),
                 'Private historical subject', 'Private historical body',
                 '<p>Private historical body</p>', 'sender@example.test',
                 'Office', '{}'::jsonb, 'preparing', $7,
                 now() + interval '1 day', $8, 1)
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(launch_id)
    .bind(revision_id)
    .bind(step_id)
    .bind(channel_id)
    .bind(connection_id)
    .bind(world.admin.principal_id)
    .bind(&export_report)
    .fetch_one(tx.conn())
    .await
    .map_err(infrastructure::error::map_sqlx)?;

    let normalized_address: String = sqlx::query_scalar(
        "SELECT normalized_key FROM email_addresses
          WHERE workspace_id=$1 AND id=$2",
    )
    .bind(tx.workspace_id())
    .bind(address_id)
    .fetch_one(tx.conn())
    .await
    .map_err(infrastructure::error::map_sqlx)?;
    sqlx::query(
        "INSERT INTO marketing_export_recipients
            (workspace_id, export_id, email_address_id, outbound_message_id,
             normalized_address, variables)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(tx.workspace_id())
    .bind(export_id)
    .bind(address_id)
    .bind(outbound_id)
    .bind(normalized_address)
    .bind(json!({"recipient.email": "private@example.test"}))
    .execute(tx.conn())
    .await
    .map_err(infrastructure::error::map_sqlx)?;

    let operation_id: Uuid = sqlx::query_scalar(
        "INSERT INTO marketing_operations
            (workspace_id, export_id, kind, correlation_key, request_hash, status,
             error_code, completed_at)
         VALUES ($1, $2, 'send_campaign', $3,
                 decode(repeat('55', 32), 'hex'), 'unknown',
                 'PROVIDER_RETIRED', now())
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(export_id)
    .bind(format!("retired-send:{export_id}"))
    .fetch_one(tx.conn())
    .await
    .map_err(infrastructure::error::map_sqlx)?;
    sqlx::query(
        "UPDATE marketing_exports SET state='syncing'
          WHERE workspace_id=$1 AND id=$2",
    )
    .bind(tx.workspace_id())
    .bind(export_id)
    .execute(tx.conn())
    .await
    .map_err(infrastructure::error::map_sqlx)?;
    sqlx::query(
        "UPDATE marketing_exports SET state='unknown'
          WHERE workspace_id=$1 AND id=$2",
    )
    .bind(tx.workspace_id())
    .bind(export_id)
    .execute(tx.conn())
    .await
    .map_err(infrastructure::error::map_sqlx)?;

    Ok((launch_id, export_id, operation_id, outbound_id))
}

#[tokio::test]
async fn retired_unknown_export_allows_local_redaction_without_resolving_journal() {
    let Some(api_db) = common::test_db().await else {
        return;
    };
    let maintenance_db = retention_db().await;
    let world = seed_workspace(&api_db).await.unwrap();
    let owner = Db::connect(
        &std::env::var("DATABASE_URL").expect("DATABASE_URL must target an isolated test DB"),
        None,
    )
    .await
    .unwrap();

    let (trusted_contact, trusted_email, untrusted_contact, untrusted_email) = {
        let mut tx = owner.begin(world.workspace_id).await.unwrap();
        let trusted_contact = compliance_fixtures::insert_contact(&mut tx, "Retired export owner")
            .await
            .unwrap();
        let trusted_email = compliance_fixtures::insert_email_address(
            &mut tx,
            &format!("retired+{}@example.test", Uuid::new_v4()),
        )
        .await
        .unwrap();
        compliance_fixtures::link_contact_email(&mut tx, trusted_contact, trusted_email)
            .await
            .unwrap();
        let untrusted_contact =
            compliance_fixtures::insert_contact(&mut tx, "Untrusted export owner")
                .await
                .unwrap();
        let untrusted_email = compliance_fixtures::insert_email_address(
            &mut tx,
            &format!("untrusted+{}@example.test", Uuid::new_v4()),
        )
        .await
        .unwrap();
        compliance_fixtures::link_contact_email(&mut tx, untrusted_contact, untrusted_email)
            .await
            .unwrap();

        let connection_id: Uuid = sqlx::query_scalar(
            "INSERT INTO integration_connections
                (workspace_id, provider, region, secret_ref, scopes, status, connected_by)
             VALUES ($1, 'zoho_campaigns', 'com', $2, ARRAY[]::text[],
                     'disabled', $3)
             RETURNING id",
        )
        .bind(world.workspace_id)
        .bind(format!("retired-history:{}", Uuid::new_v4()))
        .bind(world.admin.principal_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        sqlx::query("INSERT INTO marketing_connections (id, workspace_id) VALUES ($1, $2)")
            .bind(connection_id)
            .bind(world.workspace_id)
            .execute(tx.conn())
            .await
            .unwrap();
        let sender_identity_id: Uuid = sqlx::query_scalar(
            "INSERT INTO sender_identities
                (workspace_id, mail_account_id, address, display_name, status, verified_at)
             VALUES ($1, $2, 'retired@example.test', 'Retired', 'verified', now())
             RETURNING id",
        )
        .bind(world.workspace_id)
        .bind(world.mail_account_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let channel_id: Uuid = sqlx::query_scalar(
            "INSERT INTO delivery_channels
                (workspace_id, mail_account_id, sender_identity_id, connection_id,
                 provider, purpose, name, policy_status, enabled)
             VALUES ($1, $2, $3, $4, 'zoho_campaigns', 'marketing',
                     'Retired native channel', 'blocked', false)
             RETURNING id",
        )
        .bind(world.workspace_id)
        .bind(world.mail_account_id)
        .bind(sender_identity_id)
        .bind(connection_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();

        let campaign_id = campaigns::insert_campaign(
            &mut tx,
            world.mail_account_id,
            "Retired history campaign",
            None,
            world.admin.principal_id,
        )
        .await
        .unwrap();
        let template_id: Uuid = sqlx::query_scalar(
            "INSERT INTO email_templates (workspace_id, name, created_by)
             VALUES ($1, 'Retired history template', $2) RETURNING id",
        )
        .bind(world.workspace_id)
        .bind(world.admin.principal_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let template_version_id: Uuid = sqlx::query_scalar(
            "INSERT INTO template_versions
                (workspace_id, template_id, version_no, subject, body_text,
                 body_html, variable_schema, content_hash, created_by)
             VALUES ($1, $2, 1, 'Private subject', 'Private body', NULL,
                     '{}'::jsonb, decode(repeat('66', 32), 'hex'), $3)
             RETURNING id",
        )
        .bind(world.workspace_id)
        .bind(template_id)
        .bind(world.admin.principal_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let revision_id = campaigns::insert_revision(
            &mut tx,
            campaign_id,
            world.mail_account_id,
            channel_id,
            1,
            "UTC",
            &json!({
                "weekdays": [1, 2, 3, 4, 5, 6, 7],
                "window_start": "09:00",
                "window_end": "17:00"
            }),
            true,
            true,
            world.admin.principal_id,
        )
        .await
        .unwrap();
        let step_id = campaigns::insert_step(&mut tx, revision_id, template_version_id, 1, 0)
            .await
            .unwrap();

        let trusted = insert_historical_export(
            &mut tx,
            &world,
            connection_id,
            channel_id,
            sender_identity_id,
            revision_id,
            step_id,
            trusted_email,
            trusted_contact,
            true,
        )
        .await
        .unwrap();
        let untrusted = insert_historical_export(
            &mut tx,
            &world,
            connection_id,
            channel_id,
            sender_identity_id,
            revision_id,
            step_id,
            untrusted_email,
            untrusted_contact,
            false,
        )
        .await
        .unwrap();
        assert!(
            campaigns::freeze_revision(&mut tx, revision_id)
                .await
                .unwrap()
        );
        for (launch_id, _, _, outbound_id) in [trusted, untrusted] {
            let manifest_hash = [0x77; 32];
            assert_eq!(
                campaigns::freeze_launch_row(&mut tx, launch_id, &manifest_hash)
                    .await
                    .unwrap(),
                1
            );
            sqlx::query(
                "UPDATE campaign_launches SET status='canceled'
                  WHERE workspace_id=$1 AND id=$2",
            )
            .bind(world.workspace_id)
            .bind(launch_id)
            .execute(tx.conn())
            .await
            .unwrap();
            sqlx::query(
                "UPDATE campaign_enrollments
                    SET status='stopped', next_send_at=NULL,
                        stop_reason='provider_retired'
                  WHERE workspace_id=$1 AND launch_id=$2",
            )
            .bind(world.workspace_id)
            .bind(launch_id)
            .execute(tx.conn())
            .await
            .unwrap();
            sqlx::query(
                "UPDATE outbound_messages
                    SET status='unknown', failure_code='PROVIDER_RETIRED'
                  WHERE workspace_id=$1 AND id=$2",
            )
            .bind(world.workspace_id)
            .bind(outbound_id)
            .execute(tx.conn())
            .await
            .unwrap();
        }
        tx.commit().await.unwrap();
        (
            trusted_contact,
            trusted_email,
            untrusted_contact,
            untrusted_email,
        )
    };

    let storage_dir = tempfile::tempdir().unwrap();
    let object_store = LocalObjectStore::new(storage_dir.path(), [7; 32], b"test-key".to_vec());
    let cache = NoopCache;
    let trusted_report = erase_contact(
        &maintenance_db,
        ErasureRequest {
            workspace_id: world.workspace_id,
            contact_id: trusted_contact,
            actor_principal_id: Some(world.admin.principal_id),
            request_id: format!("retired-erasure-{}", Uuid::new_v4()),
            trace_id: None,
        },
        &object_store,
        &cache,
    )
    .await
    .unwrap();
    assert_eq!(trusted_report.marketing_exports_redacted, 1);

    let mut tx = owner.begin(world.workspace_id).await.unwrap();
    let (trusted_state, trusted_report_json): (String, Value) = sqlx::query_as(
        "SELECT e.state, e.report FROM marketing_exports e
          JOIN marketing_export_recipients r
            ON r.workspace_id=e.workspace_id AND r.export_id=e.id
          WHERE e.workspace_id=$1 AND r.email_address_id=$2",
    )
    .bind(world.workspace_id)
    .bind(trusted_email)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let (operation_status, recipient_values, recipient_sync_status, outbound_status): (
        String,
        Value,
        String,
        String,
    ) = sqlx::query_as(
        "SELECT o.status, r.variables, r.sync_status, b.status
           FROM marketing_operations o
           JOIN marketing_export_recipients r
             ON r.workspace_id=o.workspace_id AND r.export_id=o.export_id
           JOIN outbound_messages b
             ON b.workspace_id=r.workspace_id AND b.id=r.outbound_message_id
          WHERE o.workspace_id=$1 AND r.email_address_id=$2",
    )
    .bind(world.workspace_id)
    .bind(trusted_email)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(trusted_state, "canceled");
    assert_eq!(trusted_report_json["privacy_erased"], true);
    assert_eq!(trusted_report_json["provider_retired"], true);
    assert_eq!(trusted_report_json["provider_cancel_not_requested"], true);
    assert_eq!(operation_status, "unknown");
    assert_eq!(recipient_values, json!({}));
    assert_eq!(recipient_sync_status, "excluded");
    assert_eq!(outbound_status, "unknown");
    tx.rollback().await.unwrap();

    let error = erase_contact(
        &maintenance_db,
        ErasureRequest {
            workspace_id: world.workspace_id,
            contact_id: untrusted_contact,
            actor_principal_id: Some(world.admin.principal_id),
            request_id: format!("untrusted-erasure-{}", Uuid::new_v4()),
            trace_id: None,
        },
        &object_store,
        &cache,
    )
    .await
    .expect_err("an export without trusted retirement flags must stay fail-closed");
    assert_eq!(error.code, ErrorCode::Conflict);

    let mut tx = owner.begin(world.workspace_id).await.unwrap();
    let untrusted_exists: i64 =
        sqlx::query_scalar("SELECT count(*) FROM contacts WHERE workspace_id=$1 AND id=$2")
            .bind(world.workspace_id)
            .bind(untrusted_contact)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    let (untrusted_state, untrusted_operation): (String, String) = sqlx::query_as(
        "SELECT e.state, o.status
           FROM marketing_exports e
           JOIN marketing_operations o
             ON o.workspace_id=e.workspace_id AND o.export_id=e.id
           JOIN marketing_export_recipients r
             ON r.workspace_id=e.workspace_id AND r.export_id=e.id
          WHERE e.workspace_id=$1 AND r.email_address_id=$2",
    )
    .bind(world.workspace_id)
    .bind(untrusted_email)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(untrusted_exists, 1);
    assert_eq!(untrusted_state, "unknown");
    assert_eq!(untrusted_operation, "unknown");
    tx.rollback().await.unwrap();
}
