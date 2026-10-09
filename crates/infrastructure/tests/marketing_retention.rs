//! T37: privacy erasure scrubs native Campaigns snapshots that contain a
//! recipient's address or booking URL while preserving unrelated/shared data.

mod common;

use async_trait::async_trait;
use chrono::{Duration, Utc};
use contracts::{AppError, ErrorCode};
use domain::marketing::{MarketingExportState, OperationClaim, OperationResolution};
use infrastructure::{
    db::{Db, ROLE_API, ROLE_RETENTION, ROLE_WORKER, Tx},
    repo::{campaigns, marketing_exports as exports},
    retention::{CacheInvalidator, ErasureRequest, erase_contact},
    storage::LocalObjectStore,
    testing::{TestWorld, seed_workspace},
};
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Default)]
struct CacheSpy;

#[async_trait]
impl CacheInvalidator for CacheSpy {
    async fn invalidate_contact(
        &self,
        _workspace_id: Uuid,
        _contact_id: Uuid,
        _email_address_ids: &[Uuid],
    ) -> Result<(), AppError> {
        Ok(())
    }
}

struct NativeFixture {
    erased_contact_id: Uuid,
    kept_contact_id: Uuid,
    exclusive_email_id: Uuid,
    shared_email_id: Uuid,
    kept_email_id: Uuid,
    exclusive_address: String,
    shared_address: String,
    kept_address: String,
    mixed_launch_id: Uuid,
    shared_launch_id: Uuid,
    erased_shared_launch_id: Uuid,
    mixed_export_id: Uuid,
    shared_export_id: Uuid,
    erased_shared_export_id: Uuid,
    exclusive_outbound_id: Uuid,
    kept_outbound_id: Uuid,
    shared_outbound_id: Uuid,
    erased_shared_outbound_id: Uuid,
    mixed_manifest_hash: Vec<u8>,
    shared_manifest_hash: Vec<u8>,
    kept_variables: Value,
    shared_variables: Value,
    exclusive_variables: Value,
    exclusive_booking_url: String,
    original_export_hash: Vec<u8>,
    original_content_hash: Vec<u8>,
    original_export_subject: String,
    original_export_text: String,
    original_export_html: String,
    original_field_map: Value,
    original_report: Value,
}

async fn insert_template_version(tx: &mut Tx<'_>, created_by: Uuid) -> Uuid {
    let template_id: Uuid = sqlx::query_scalar(
        "INSERT INTO email_templates (workspace_id, name, created_by)
         VALUES ($1, 'retention fixture template', $2) RETURNING id",
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
         VALUES ($1, $2, 1, 'Template subject', 'Template body', '{}',
                 decode(repeat('22', 32), 'hex'), $3)
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(template_id)
    .bind(created_by)
    .fetch_one(tx.conn())
    .await
    .unwrap()
}

async fn insert_email_address(tx: &mut Tx<'_>, address: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO email_addresses
            (workspace_id, address_original, normalized_key, kind, verification_status, verified_at)
         VALUES ($1, $2, lower($2), 'person', 'verified', now()) RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(address)
    .fetch_one(tx.conn())
    .await
    .unwrap()
}

async fn insert_link(tx: &mut Tx<'_>, contact_id: Uuid, email_address_id: Uuid) {
    sqlx::query(
        "INSERT INTO contact_emails (workspace_id, contact_id, email_address_id, is_primary)
         VALUES ($1, $2, $3, false)",
    )
    .bind(tx.workspace_id())
    .bind(contact_id)
    .bind(email_address_id)
    .execute(tx.conn())
    .await
    .unwrap();
}

#[allow(clippy::too_many_arguments)]
async fn insert_outbound(
    tx: &mut Tx<'_>,
    world: &TestWorld,
    channel_id: Uuid,
    sender_identity_id: Uuid,
    launch_id: Uuid,
    revision_id: Uuid,
    step_id: Uuid,
    email_address_id: Uuid,
    enrollment_id: Uuid,
    recipient_address: &str,
    subject: &str,
    body_text: &str,
    body_html: &str,
    hash: &[u8; 32],
) -> Uuid {
    campaigns::insert_step_outbound(
        tx,
        world.mail_account_id,
        channel_id,
        sender_identity_id,
        email_address_id,
        None,
        launch_id,
        revision_id,
        enrollment_id,
        step_id,
        world.admin.principal_id,
        "sender@example.test",
        recipient_address,
        subject,
        body_text,
        Some(body_html),
        hash,
    )
    .await
    .unwrap()
}

#[allow(clippy::too_many_arguments)]
fn export_input(
    world: &TestWorld,
    launch_id: Uuid,
    revision_id: Uuid,
    step_id: Uuid,
    delivery_channel_id: Uuid,
    source_manifest_hash: &[u8],
    export_hash: &[u8; 32],
    content_hash: &[u8; 32],
    subject: &str,
    body_text: &str,
    body_html: &str,
    field_map: Value,
) -> exports::NewMarketingExport {
    exports::NewMarketingExport {
        launch_id,
        revision_id,
        step_id,
        delivery_channel_id,
        connection_id: world.connection_id,
        source_manifest_hash: source_manifest_hash.to_vec(),
        content_hash: content_hash.to_vec(),
        audience_hash: vec![0x37; 32],
        export_hash: export_hash.to_vec(),
        subject: subject.to_string(),
        body_text: body_text.to_string(),
        body_html: body_html.to_string(),
        sender_address: "sender@example.test".to_string(),
        sender_name: "Marketing Team".to_string(),
        field_map,
        prepared_by: world.admin.principal_id,
        expires_at: Utc::now() + Duration::days(1),
    }
}

async fn create_native_fixture(db: &Db, world: &TestWorld) -> NativeFixture {
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let erased_contact_id: Uuid = sqlx::query_scalar(
        "INSERT INTO contacts (workspace_id, name) VALUES ($1, 'Erased Recipient') RETURNING id",
    )
    .bind(world.workspace_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let kept_contact_id: Uuid = sqlx::query_scalar(
        "INSERT INTO contacts (workspace_id, name) VALUES ($1, 'Control Recipient') RETURNING id",
    )
    .bind(world.workspace_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let suffix = Uuid::new_v4();
    let exclusive_address = format!("erased+{suffix}@example.test");
    let shared_address = format!("shared+{suffix}@example.test");
    let kept_address = format!("kept+{suffix}@example.test");
    let exclusive_email_id = insert_email_address(&mut tx, &exclusive_address).await;
    let shared_email_id = insert_email_address(&mut tx, &shared_address).await;
    let kept_email_id = insert_email_address(&mut tx, &kept_address).await;
    insert_link(&mut tx, erased_contact_id, exclusive_email_id).await;
    insert_link(&mut tx, erased_contact_id, shared_email_id).await;
    insert_link(&mut tx, kept_contact_id, shared_email_id).await;
    insert_link(&mut tx, kept_contact_id, kept_email_id).await;

    let sender_identity_id: Uuid = sqlx::query_scalar(
        "INSERT INTO sender_identities
            (workspace_id, mail_account_id, address, display_name, status, verified_at)
         VALUES ($1, $2, $3, 'Marketing Team', 'verified', now()) RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(world.mail_account_id)
    .bind(format!("sender+{suffix}@test.dev"))
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let delivery_channel_id: Uuid = sqlx::query_scalar(
        "INSERT INTO delivery_channels
            (workspace_id, mail_account_id, sender_identity_id, connection_id,
             provider, purpose, name)
         VALUES ($1,$2,$3,$4,'zoho_mail','marketing','Campaigns retention fixture')
         RETURNING id",
    )
    .bind(world.workspace_id)
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
        "Retention fixture campaign",
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
    let mixed_launch_id = campaigns::insert_launch(
        &mut tx,
        revision_id,
        delivery_channel_id,
        world.admin.principal_id,
        Utc::now() + Duration::days(1),
    )
    .await
    .unwrap();
    let shared_launch_id = campaigns::insert_launch(
        &mut tx,
        revision_id,
        delivery_channel_id,
        world.admin.principal_id,
        Utc::now() + Duration::days(1),
    )
    .await
    .unwrap();
    let erased_shared_launch_id = campaigns::insert_launch(
        &mut tx,
        revision_id,
        delivery_channel_id,
        world.admin.principal_id,
        Utc::now() + Duration::days(1),
    )
    .await
    .unwrap();
    let exclusive_enrollment = campaigns::insert_enrollment(
        &mut tx,
        mixed_launch_id,
        revision_id,
        exclusive_email_id,
        Some(erased_contact_id),
        "pending",
        None,
    )
    .await
    .unwrap();
    let kept_enrollment = campaigns::insert_enrollment(
        &mut tx,
        mixed_launch_id,
        revision_id,
        kept_email_id,
        Some(kept_contact_id),
        "pending",
        None,
    )
    .await
    .unwrap();
    let shared_enrollment = campaigns::insert_enrollment(
        &mut tx,
        shared_launch_id,
        revision_id,
        shared_email_id,
        Some(kept_contact_id),
        "pending",
        None,
    )
    .await
    .unwrap();
    // The address is shared with the retained contact, but this frozen
    // enrollment explicitly belongs to the erased contact.
    let erased_shared_enrollment = campaigns::insert_enrollment(
        &mut tx,
        erased_shared_launch_id,
        revision_id,
        shared_email_id,
        Some(erased_contact_id),
        "pending",
        None,
    )
    .await
    .unwrap();
    let exclusive_booking_url = format!("https://booking.example.test/{suffix}/erased");
    let kept_booking_url = format!("https://booking.example.test/{suffix}/kept");
    let shared_booking_url = format!("https://booking.example.test/{suffix}/shared");
    let erased_shared_booking_url =
        format!("https://booking.example.test/{suffix}/shared-erased-owner");
    let exclusive_variables = json!({
        "recipient.email": exclusive_address,
        "meeting.booking_url": exclusive_booking_url,
        "contact.name": "Erased Recipient"
    });
    let exclusive_body = format!("Hello {exclusive_address}; booking {exclusive_booking_url}");
    let exclusive_html =
        format!("<p>{exclusive_address}</p><a href=\"{exclusive_booking_url}\">Book</a>");
    let exclusive_outbound_id = insert_outbound(
        &mut tx,
        world,
        delivery_channel_id,
        sender_identity_id,
        mixed_launch_id,
        revision_id,
        step_id,
        exclusive_email_id,
        exclusive_enrollment,
        &exclusive_address,
        "Erased outbound",
        &exclusive_body,
        &exclusive_html,
        &[0x61; 32],
    )
    .await;
    let kept_outbound_id = insert_outbound(
        &mut tx,
        world,
        delivery_channel_id,
        sender_identity_id,
        mixed_launch_id,
        revision_id,
        step_id,
        kept_email_id,
        kept_enrollment,
        &kept_address,
        "Control outbound",
        "Control body stays",
        "<p>Control body stays</p>",
        &[0x62; 32],
    )
    .await;
    let shared_outbound_id = insert_outbound(
        &mut tx,
        world,
        delivery_channel_id,
        sender_identity_id,
        shared_launch_id,
        revision_id,
        step_id,
        shared_email_id,
        shared_enrollment,
        &shared_address,
        "Shared outbound",
        "Shared body stays",
        "<p>Shared body stays</p>",
        &[0x63; 32],
    )
    .await;
    let erased_shared_outbound_id = insert_outbound(
        &mut tx,
        world,
        delivery_channel_id,
        sender_identity_id,
        erased_shared_launch_id,
        revision_id,
        step_id,
        shared_email_id,
        erased_shared_enrollment,
        &shared_address,
        "Erased shared-address outbound",
        &format!("Erased Recipient at {shared_address}; {erased_shared_booking_url}"),
        &format!("<p>Erased Recipient</p><a href=\"{erased_shared_booking_url}\">Book</a>"),
        &[0x64; 32],
    )
    .await;

    let kept_variables = json!({
        "recipient.email": kept_address,
        "meeting.booking_url": kept_booking_url,
        "contact.name": "Control Recipient"
    });
    let shared_variables = json!({
        "recipient.email": shared_address,
        "meeting.booking_url": shared_booking_url,
        "contact.name": "Control Recipient"
    });
    let erased_shared_variables = json!({
        "recipient.email": shared_address,
        "meeting.booking_url": erased_shared_booking_url,
        "contact.name": "Erased Recipient"
    });
    exports::record_launch_values(
        &mut tx,
        mixed_launch_id,
        &[
            exports::NewLaunchValue {
                email_address_id: exclusive_email_id,
                variables: exclusive_variables.clone(),
            },
            exports::NewLaunchValue {
                email_address_id: kept_email_id,
                variables: kept_variables.clone(),
            },
        ],
    )
    .await
    .unwrap();
    exports::record_launch_values(
        &mut tx,
        shared_launch_id,
        &[exports::NewLaunchValue {
            email_address_id: shared_email_id,
            variables: shared_variables.clone(),
        }],
    )
    .await
    .unwrap();
    exports::record_launch_values(
        &mut tx,
        erased_shared_launch_id,
        &[exports::NewLaunchValue {
            email_address_id: shared_email_id,
            variables: erased_shared_variables.clone(),
        }],
    )
    .await
    .unwrap();

    let mixed_manifest_hash = [0x71; 32];
    let shared_manifest_hash = [0x72; 32];
    let erased_shared_manifest_hash = [0x73; 32];
    assert_eq!(
        campaigns::freeze_launch_row(&mut tx, mixed_launch_id, &mixed_manifest_hash)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        campaigns::freeze_launch_row(&mut tx, shared_launch_id, &shared_manifest_hash)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        campaigns::freeze_launch_row(
            &mut tx,
            erased_shared_launch_id,
            &erased_shared_manifest_hash,
        )
        .await
        .unwrap(),
        1
    );

    let exclusive_field_map = json!({
        "recipient.email": exclusive_address,
        "booking_url": exclusive_booking_url
    });
    let exclusive_report = json!({
        "recipient": exclusive_address,
        "booking_url": exclusive_booking_url,
        "open_count": 0
    });
    let original_export_subject = format!("Welcome {exclusive_address}");
    let original_export_text =
        format!("Hello {exclusive_address}, book at {exclusive_booking_url}");
    let original_export_html =
        format!("<p>{exclusive_address}</p><a href=\"{exclusive_booking_url}\">Book</a>");
    let original_export_hash = [0x81; 32].to_vec();
    let original_content_hash = [0x82; 32].to_vec();
    let mixed_export = exports::insert_export(
        &mut tx,
        export_input(
            world,
            mixed_launch_id,
            revision_id,
            step_id,
            delivery_channel_id,
            &mixed_manifest_hash,
            &[0x81; 32],
            &[0x82; 32],
            &original_export_subject,
            &original_export_text,
            &original_export_html,
            exclusive_field_map.clone(),
        ),
    )
    .await
    .unwrap();
    exports::replace_recipients(
        &mut tx,
        mixed_export.id,
        &[
            exports::NewExportRecipient {
                email_address_id: exclusive_email_id,
                outbound_message_id: exclusive_outbound_id,
                normalized_address: exclusive_address.clone(),
                variables: json!({
                    "recipient.email": exclusive_address,
                    "meeting.booking_url": exclusive_booking_url,
                    "contact.name": "Erased Recipient"
                }),
                sync_status: "active".to_string(),
            },
            exports::NewExportRecipient {
                email_address_id: kept_email_id,
                outbound_message_id: kept_outbound_id,
                normalized_address: kept_address.clone(),
                variables: kept_variables.clone(),
                sync_status: "active".to_string(),
            },
        ],
    )
    .await
    .unwrap();
    exports::update_execution_metadata(
        &mut tx,
        mixed_export.id,
        exports::ExecutionMetadata {
            provider_list_key: Some("provider-list-mixed".to_string()),
            topic_id: Some("provider-topic-mixed".to_string()),
            campaign_key: Some("provider-campaign-mixed".to_string()),
            report: Some(exclusive_report.clone()),
            recipient_count: Some(2),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        exports::claim_operation(
            &mut tx,
            mixed_export.id,
            "create_list",
            "fixture-list",
            &[0x91; 32]
        )
        .await
        .unwrap(),
        domain::marketing::OperationClaim::New
    );
    assert!(
        exports::complete_operation(
            &mut tx,
            mixed_export.id,
            "create_list",
            "fixture-list",
            Some("provider-list-mixed"),
        )
        .await
        .unwrap()
    );

    let shared_subject = format!("Shared contact {shared_address}");
    let shared_export = exports::insert_export(
        &mut tx,
        export_input(
            world,
            shared_launch_id,
            revision_id,
            step_id,
            delivery_channel_id,
            &shared_manifest_hash,
            &[0xa1; 32],
            &[0xa2; 32],
            &shared_subject,
            "Shared body remains",
            "<p>Shared body remains</p>",
            json!({"recipient.email": shared_address}),
        ),
    )
    .await
    .unwrap();
    exports::replace_recipients(
        &mut tx,
        shared_export.id,
        &[exports::NewExportRecipient {
            email_address_id: shared_email_id,
            outbound_message_id: shared_outbound_id,
            normalized_address: shared_address.clone(),
            variables: shared_variables.clone(),
            sync_status: "active".to_string(),
        }],
    )
    .await
    .unwrap();
    exports::update_execution_metadata(
        &mut tx,
        shared_export.id,
        exports::ExecutionMetadata {
            report: Some(json!({"recipient": shared_address, "opens": 3})),
            recipient_count: Some(1),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let erased_shared_export = exports::insert_export(
        &mut tx,
        export_input(
            world,
            erased_shared_launch_id,
            revision_id,
            step_id,
            delivery_channel_id,
            &erased_shared_manifest_hash,
            &[0xb1; 32],
            &[0xb2; 32],
            "Erased owner of shared address",
            &format!("Erased Recipient; {shared_address}; {erased_shared_booking_url}"),
            &format!("<p>Erased Recipient</p><a href=\"{erased_shared_booking_url}\">Book</a>"),
            json!({
                "recipient.email": shared_address,
                "contact.name": "Erased Recipient",
                "booking_url": erased_shared_booking_url
            }),
        ),
    )
    .await
    .unwrap();
    exports::replace_recipients(
        &mut tx,
        erased_shared_export.id,
        &[exports::NewExportRecipient {
            email_address_id: shared_email_id,
            outbound_message_id: erased_shared_outbound_id,
            normalized_address: shared_address.clone(),
            variables: erased_shared_variables.clone(),
            sync_status: "active".to_string(),
        }],
    )
    .await
    .unwrap();
    exports::update_execution_metadata(
        &mut tx,
        erased_shared_export.id,
        exports::ExecutionMetadata {
            report: Some(json!({
                "recipient": "Erased Recipient",
                "email": shared_address,
                "booking_url": erased_shared_booking_url
            })),
            recipient_count: Some(1),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    tx.commit().await.unwrap();
    NativeFixture {
        erased_contact_id,
        kept_contact_id,
        exclusive_email_id,
        shared_email_id,
        kept_email_id,
        exclusive_address,
        shared_address,
        kept_address,
        mixed_launch_id,
        shared_launch_id,
        erased_shared_launch_id,
        mixed_export_id: mixed_export.id,
        shared_export_id: shared_export.id,
        erased_shared_export_id: erased_shared_export.id,
        exclusive_outbound_id,
        kept_outbound_id,
        shared_outbound_id,
        erased_shared_outbound_id,
        mixed_manifest_hash: mixed_manifest_hash.to_vec(),
        shared_manifest_hash: shared_manifest_hash.to_vec(),
        kept_variables,
        shared_variables,
        exclusive_variables,
        exclusive_booking_url,
        original_export_hash,
        original_content_hash,
        original_export_subject,
        original_export_text,
        original_export_html,
        original_field_map: exclusive_field_map,
        original_report: exclusive_report,
    }
}

async fn retention_db() -> Db {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must target the privacy test DB");
    let owner = Db::connect(&url, None).await.expect("owner DB connection");
    owner.migrate().await.expect("apply migrations");
    Db::connect(&url, Some(ROLE_RETENTION))
        .await
        .expect("retention DB connection")
}

#[tokio::test]
async fn erasing_contact_scrubs_native_campaigns_snapshots_but_preserves_controls() {
    let api_db = common::test_db().await.unwrap();
    let maintenance_db = retention_db().await;
    let world = seed_workspace(&api_db).await.unwrap();
    let fixture = create_native_fixture(&api_db, &world).await;
    let storage_dir = tempfile::tempdir().unwrap();
    let object_store =
        LocalObjectStore::new(storage_dir.path(), [7; 32], b"retention-test-key".to_vec());

    // Simulate an accepted native bulk send. Privacy erasure must block
    // before the transaction commits any suppression or snapshot scrub.
    let mut send_tx = api_db.begin(world.workspace_id).await.unwrap();
    for state in [
        MarketingExportState::Syncing,
        MarketingExportState::Ready,
        MarketingExportState::Submitting,
        MarketingExportState::InProgress,
    ] {
        let current = exports::get_export(&mut send_tx, fixture.mixed_export_id)
            .await
            .unwrap()
            .unwrap();
        assert!(
            exports::update_state(&mut send_tx, current.id, current.version, state)
                .await
                .unwrap()
        );
    }
    assert_eq!(
        exports::claim_operation(
            &mut send_tx,
            fixture.mixed_export_id,
            "send_campaign",
            "fixture-send",
            &[0xa5; 32],
        )
        .await
        .unwrap(),
        OperationClaim::New
    );
    assert!(
        exports::complete_operation(
            &mut send_tx,
            fixture.mixed_export_id,
            "send_campaign",
            "fixture-send",
            Some("provider-campaign-mixed"),
        )
        .await
        .unwrap()
    );
    send_tx.commit().await.unwrap();

    let blocked = erase_contact(
        &maintenance_db,
        ErasureRequest {
            workspace_id: world.workspace_id,
            contact_id: fixture.erased_contact_id,
            actor_principal_id: Some(world.admin.principal_id),
            request_id: format!("t37-blocked-{}", Uuid::new_v4()),
            trace_id: None,
        },
        &object_store,
        &CacheSpy,
    )
    .await
    .expect_err("an accepted in-progress bulk send must be reconciled first");
    assert_eq!(blocked.code, ErrorCode::Conflict);
    let mut unchanged_tx = api_db.begin(world.workspace_id).await.unwrap();
    let still_present: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM contacts WHERE workspace_id = $1 AND id = $2)",
    )
    .bind(world.workspace_id)
    .bind(fixture.erased_contact_id)
    .fetch_one(unchanged_tx.conn())
    .await
    .unwrap();
    assert!(
        still_present,
        "blocked erasure must not partially delete the contact"
    );
    let suppressions_before_resolution: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM suppressions
          WHERE workspace_id = $1 AND email_address_id = $2 AND purpose_scope = 'all'",
    )
    .bind(world.workspace_id)
    .bind(fixture.exclusive_email_id)
    .fetch_one(unchanged_tx.conn())
    .await
    .unwrap();
    assert_eq!(
        suppressions_before_resolution, 0,
        "blocked erasure must roll back suppression writes"
    );
    let unchanged_export = exports::get_export(&mut unchanged_tx, fixture.mixed_export_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(unchanged_export.subject, fixture.original_export_subject);
    assert_eq!(unchanged_export.body_text, fixture.original_export_text);
    let unchanged_values = exports::list_launch_values(&mut unchanged_tx, fixture.mixed_launch_id)
        .await
        .unwrap();
    assert_eq!(
        unchanged_values
            .iter()
            .find(|value| value.email_address_id == fixture.exclusive_email_id)
            .unwrap()
            .variables,
        fixture.exclusive_variables
    );
    unchanged_tx.rollback().await.unwrap();

    // Provider completion is reconciled; erasure may then redact local data
    // while explicitly reporting that no remote cancellation was requested.
    let mut completed_tx = api_db.begin(world.workspace_id).await.unwrap();
    let active_export = exports::get_export(&mut completed_tx, fixture.mixed_export_id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        exports::update_state(
            &mut completed_tx,
            active_export.id,
            active_export.version,
            MarketingExportState::Completed,
        )
        .await
        .unwrap()
    );
    completed_tx.commit().await.unwrap();

    let report = erase_contact(
        &maintenance_db,
        ErasureRequest {
            workspace_id: world.workspace_id,
            contact_id: fixture.erased_contact_id,
            actor_principal_id: Some(world.admin.principal_id),
            request_id: format!("t37-{}", Uuid::new_v4()),
            trace_id: None,
        },
        &object_store,
        &CacheSpy,
    )
    .await
    .unwrap();

    assert_eq!(report.marketing_exports_redacted, 2);
    assert!(report.provider_cancel_not_requested);
    assert_eq!(report.email_addresses_suppressed, 1);

    let mut tx = api_db.begin(world.workspace_id).await.unwrap();
    let erased_contact_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM contacts WHERE workspace_id = $1 AND id = $2)",
    )
    .bind(world.workspace_id)
    .bind(fixture.erased_contact_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert!(!erased_contact_exists);

    let suppression = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM suppressions
          WHERE workspace_id = $1 AND email_address_id = $2
            AND purpose_scope = 'all' AND released_at IS NULL",
    )
    .bind(world.workspace_id)
    .bind(fixture.exclusive_email_id)
    .fetch_optional(tx.conn())
    .await
    .unwrap();
    assert!(suppression.is_some(), "suppression identity must remain");
    let retained_address: String = sqlx::query_scalar(
        "SELECT address_original FROM email_addresses WHERE workspace_id = $1 AND id = $2",
    )
    .bind(world.workspace_id)
    .bind(fixture.exclusive_email_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(retained_address, fixture.exclusive_address);

    let shared_links: Vec<Uuid> = sqlx::query_scalar(
        "SELECT contact_id FROM contact_emails
          WHERE workspace_id = $1 AND email_address_id = $2 ORDER BY contact_id",
    )
    .bind(world.workspace_id)
    .bind(fixture.shared_email_id)
    .fetch_all(tx.conn())
    .await
    .unwrap();
    assert_eq!(shared_links, vec![fixture.kept_contact_id]);

    let launch_values = exports::list_launch_values(&mut tx, fixture.mixed_launch_id)
        .await
        .unwrap();
    let erased_launch_value = launch_values
        .iter()
        .find(|value| value.email_address_id == fixture.exclusive_email_id)
        .unwrap();
    assert_eq!(erased_launch_value.variables, json!({}));
    let kept_launch_value = launch_values
        .iter()
        .find(|value| value.email_address_id == fixture.kept_email_id)
        .unwrap();
    assert_eq!(kept_launch_value.variables, fixture.kept_variables);
    let shared_launch_values = exports::list_launch_values(&mut tx, fixture.shared_launch_id)
        .await
        .unwrap();
    assert_eq!(shared_launch_values.len(), 1);
    assert_eq!(shared_launch_values[0].variables, fixture.shared_variables);
    let erased_shared_launch_values =
        exports::list_launch_values(&mut tx, fixture.erased_shared_launch_id)
            .await
            .unwrap();
    assert_eq!(erased_shared_launch_values.len(), 1);
    assert_eq!(erased_shared_launch_values[0].variables, json!({}));

    let redacted_export = exports::get_export(&mut tx, fixture.mixed_export_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(redacted_export.state, "canceled");
    assert_eq!(redacted_export.subject, "");
    assert_eq!(redacted_export.body_text, "");
    assert_eq!(redacted_export.body_html, "");
    assert_eq!(redacted_export.field_map, json!({}));
    assert_eq!(redacted_export.source_manifest_hash, vec![0; 32]);
    assert_eq!(redacted_export.content_hash, vec![0; 32]);
    assert_eq!(redacted_export.audience_hash, vec![0; 32]);
    assert_eq!(redacted_export.export_hash, vec![0; 32]);
    assert_eq!(redacted_export.recipient_count, 0);
    assert_eq!(
        redacted_export.report,
        json!({"privacy_erased": true, "provider_cancel_not_requested": true})
    );
    assert_eq!(
        redacted_export.provider_list_key.as_deref(),
        Some("provider-list-mixed")
    );
    assert_eq!(
        redacted_export.campaign_key.as_deref(),
        Some("provider-campaign-mixed")
    );

    let mixed_recipients = exports::list_recipients(&mut tx, fixture.mixed_export_id)
        .await
        .unwrap();
    let erased_recipient = mixed_recipients
        .iter()
        .find(|recipient| recipient.email_address_id == fixture.exclusive_email_id)
        .unwrap();
    assert_eq!(
        erased_recipient.normalized_address,
        format!("privacy-erased:{}", fixture.exclusive_email_id)
    );
    assert_eq!(erased_recipient.variables, json!({}));
    assert_eq!(erased_recipient.sync_status, "excluded");
    let kept_recipient = mixed_recipients
        .iter()
        .find(|recipient| recipient.email_address_id == fixture.kept_email_id)
        .unwrap();
    assert_eq!(kept_recipient.normalized_address, fixture.kept_address);
    assert_eq!(kept_recipient.variables, fixture.kept_variables);
    assert_eq!(kept_recipient.sync_status, "active");

    let shared_export = exports::get_export(&mut tx, fixture.shared_export_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(shared_export.state, "preparing");
    assert_eq!(
        shared_export.subject,
        format!("Shared contact {}", fixture.shared_address)
    );
    assert_eq!(shared_export.content_hash, vec![0xa2; 32]);
    assert_eq!(
        shared_export.report,
        json!({"recipient": fixture.shared_address, "opens": 3})
    );
    let shared_recipients = exports::list_recipients(&mut tx, fixture.shared_export_id)
        .await
        .unwrap();
    assert_eq!(shared_recipients.len(), 1);
    assert_eq!(
        shared_recipients[0].normalized_address,
        fixture.shared_address
    );
    assert_eq!(shared_recipients[0].variables, fixture.shared_variables);

    let erased_shared_export = exports::get_export(&mut tx, fixture.erased_shared_export_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(erased_shared_export.state, "canceled");
    assert_eq!(erased_shared_export.subject, "");
    assert_eq!(erased_shared_export.body_text, "");
    assert_eq!(erased_shared_export.body_html, "");
    assert_eq!(erased_shared_export.field_map, json!({}));
    assert_eq!(erased_shared_export.report["privacy_erased"], true);
    assert_eq!(erased_shared_export.content_hash, vec![0; 32]);
    let erased_shared_recipients =
        exports::list_recipients(&mut tx, fixture.erased_shared_export_id)
            .await
            .unwrap();
    assert_eq!(erased_shared_recipients.len(), 1);
    assert_eq!(
        erased_shared_recipients[0].normalized_address,
        format!("privacy-erased:{}", fixture.shared_email_id)
    );
    assert_eq!(erased_shared_recipients[0].variables, json!({}));

    let operations = exports::list_operations(&mut tx, fixture.mixed_export_id)
        .await
        .unwrap();
    assert_eq!(operations.len(), 2);
    let list_operation = operations
        .iter()
        .find(|operation| operation.kind == "create_list")
        .unwrap();
    assert_eq!(list_operation.status, "completed");
    assert_eq!(
        list_operation.provider_id.as_deref(),
        Some("provider-list-mixed")
    );
    let send_operation = operations
        .iter()
        .find(|operation| operation.kind == "send_campaign")
        .unwrap();
    assert_eq!(send_operation.status, "completed");
    assert_eq!(
        send_operation.provider_id.as_deref(),
        Some("provider-campaign-mixed")
    );

    let outbound_data: (String, String, String, Vec<u8>) = sqlx::query_as(
        "SELECT to_address_snapshot, subject, body_text, payload_hash
           FROM outbound_messages WHERE workspace_id = $1 AND id = $2",
    )
    .bind(world.workspace_id)
    .bind(fixture.exclusive_outbound_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(outbound_data.0, "[erased]");
    assert_eq!(outbound_data.1, "[erased]");
    assert_eq!(outbound_data.2, "");
    assert_eq!(outbound_data.3, vec![0; 32]);
    let kept_outbound_body: String = sqlx::query_scalar(
        "SELECT body_text FROM outbound_messages WHERE workspace_id = $1 AND id = $2",
    )
    .bind(world.workspace_id)
    .bind(fixture.kept_outbound_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(kept_outbound_body, "Control body stays");
    let shared_outbound_body: String = sqlx::query_scalar(
        "SELECT body_text FROM outbound_messages WHERE workspace_id = $1 AND id = $2",
    )
    .bind(world.workspace_id)
    .bind(fixture.shared_outbound_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(shared_outbound_body, "Shared body stays");
    let erased_shared_outbound: (String, String) = sqlx::query_as(
        "SELECT to_address_snapshot, body_text FROM outbound_messages
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(world.workspace_id)
    .bind(fixture.erased_shared_outbound_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(erased_shared_outbound.0, "[erased]");
    assert_eq!(erased_shared_outbound.1, "");
    tx.commit().await.unwrap();

    assert_eq!(
        fixture.original_export_hash,
        vec![0x81; 32],
        "fixture retains the pre-erasure hash for the regression assertion"
    );
    assert_eq!(fixture.original_content_hash, vec![0x82; 32]);
    assert!(
        fixture
            .original_export_subject
            .contains(&fixture.exclusive_address)
    );
    assert!(
        fixture
            .original_export_text
            .contains(&fixture.exclusive_booking_url)
    );
    assert!(
        fixture
            .original_export_html
            .contains(&fixture.exclusive_booking_url)
    );
    assert!(
        fixture
            .original_field_map
            .to_string()
            .contains(&fixture.exclusive_address)
    );
    assert!(
        fixture
            .original_report
            .to_string()
            .contains(&fixture.exclusive_address)
    );
    assert_eq!(fixture.mixed_manifest_hash, vec![0x71; 32]);
    assert_eq!(fixture.shared_manifest_hash, vec![0x72; 32]);
}

#[tokio::test]
async fn api_and_worker_cannot_use_retention_guc_to_restore_erased_pii() {
    let api_db = common::test_db().await.unwrap();
    let maintenance_db = retention_db().await;
    let world = seed_workspace(&api_db).await.unwrap();
    let fixture = create_native_fixture(&api_db, &world).await;
    let storage_dir = tempfile::tempdir().unwrap();
    let object_store =
        LocalObjectStore::new(storage_dir.path(), [8; 32], b"retention-test-key".to_vec());
    erase_contact(
        &maintenance_db,
        ErasureRequest {
            workspace_id: world.workspace_id,
            contact_id: fixture.erased_contact_id,
            actor_principal_id: Some(world.admin.principal_id),
            request_id: format!("t37-guc-{}", Uuid::new_v4()),
            trace_id: None,
        },
        &object_store,
        &CacheSpy,
    )
    .await
    .unwrap();

    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must target the privacy test DB");
    for role in [ROLE_API, ROLE_WORKER] {
        let scoped_db = Db::connect(&url, Some(role)).await.unwrap();
        let mut tx = scoped_db.begin(world.workspace_id).await.unwrap();
        let setting: String =
            sqlx::query_scalar("SELECT set_config('app.retention_operator', 'on', true)")
                .fetch_one(tx.conn())
                .await
                .unwrap();
        assert_eq!(setting, "on");

        sqlx::query("SAVEPOINT forbidden_launch_values_rewrite")
            .execute(tx.conn())
            .await
            .unwrap();
        let value_rewrite = sqlx::query(
            "UPDATE marketing_launch_values SET variables = '{\"restored\": true}'::jsonb
              WHERE workspace_id = $1 AND launch_id = $2 AND email_address_id = $3",
        )
        .bind(world.workspace_id)
        .bind(fixture.mixed_launch_id)
        .bind(fixture.exclusive_email_id)
        .execute(tx.conn())
        .await;
        sqlx::query("ROLLBACK TO SAVEPOINT forbidden_launch_values_rewrite")
            .execute(tx.conn())
            .await
            .unwrap();
        assert!(
            value_rewrite
                .expect_err("runtime roles cannot use the retention bypass")
                .to_string()
                .contains("immutable")
        );

        sqlx::query("SAVEPOINT forbidden_export_refresh")
            .execute(tx.conn())
            .await
            .unwrap();
        let export_refresh = sqlx::query(
            "UPDATE marketing_exports SET report = '{\"restored\": true}'::jsonb
              WHERE workspace_id = $1 AND id = $2",
        )
        .bind(world.workspace_id)
        .bind(fixture.mixed_export_id)
        .execute(tx.conn())
        .await;
        sqlx::query("ROLLBACK TO SAVEPOINT forbidden_export_refresh")
            .execute(tx.conn())
            .await
            .unwrap();
        assert!(
            export_refresh.is_err(),
            "{role} must not repopulate a redacted export"
        );

        sqlx::query("SAVEPOINT forbidden_new_operation")
            .execute(tx.conn())
            .await
            .unwrap();
        let operation_insert = sqlx::query(
            "INSERT INTO marketing_operations
                (workspace_id, export_id, kind, correlation_key, request_hash)
             VALUES ($1, $2, 'send_campaign', 'after-erasure', decode(repeat('55', 32), 'hex'))",
        )
        .bind(world.workspace_id)
        .bind(fixture.mixed_export_id)
        .execute(tx.conn())
        .await;
        sqlx::query("ROLLBACK TO SAVEPOINT forbidden_new_operation")
            .execute(tx.conn())
            .await
            .unwrap();
        assert!(
            operation_insert.is_err(),
            "{role} cannot start a provider write for a redacted export"
        );
        tx.rollback().await.unwrap();
    }
}

#[tokio::test]
async fn started_campaign_sync_blocks_until_provider_rejects_the_write() {
    let api_db = common::test_db().await.unwrap();
    let maintenance_db = retention_db().await;
    let world = seed_workspace(&api_db).await.unwrap();
    let fixture = create_native_fixture(&api_db, &world).await;
    let mut tx = api_db.begin(world.workspace_id).await.unwrap();
    assert_eq!(
        exports::claim_operation(
            &mut tx,
            fixture.mixed_export_id,
            "subscribe",
            "pending-subscribe",
            &[0xc1; 32],
        )
        .await
        .unwrap(),
        OperationClaim::New
    );
    tx.commit().await.unwrap();

    let storage_dir = tempfile::tempdir().unwrap();
    let object_store =
        LocalObjectStore::new(storage_dir.path(), [10; 32], b"retention-test-key".to_vec());
    let blocked = erase_contact(
        &maintenance_db,
        ErasureRequest {
            workspace_id: world.workspace_id,
            contact_id: fixture.erased_contact_id,
            actor_principal_id: Some(world.admin.principal_id),
            request_id: format!("t37-sync-{}", Uuid::new_v4()),
            trace_id: None,
        },
        &object_store,
        &CacheSpy,
    )
    .await
    .expect_err("an in-flight contact sync must be resolved before erasure");
    assert_eq!(blocked.code, ErrorCode::Conflict);
    let mut unchanged_tx = api_db.begin(world.workspace_id).await.unwrap();
    let suppression_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM suppressions
          WHERE workspace_id = $1 AND email_address_id = $2 AND purpose_scope = 'all'",
    )
    .bind(world.workspace_id)
    .bind(fixture.exclusive_email_id)
    .fetch_one(unchanged_tx.conn())
    .await
    .unwrap();
    assert_eq!(
        suppression_count, 0,
        "blocked sync must not partially erase"
    );
    let unchanged_export = exports::get_export(&mut unchanged_tx, fixture.mixed_export_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(unchanged_export.subject, fixture.original_export_subject);
    unchanged_tx.rollback().await.unwrap();

    let mut resolve_tx = api_db.begin(world.workspace_id).await.unwrap();
    assert!(
        exports::mark_operation_rejected(
            &mut resolve_tx,
            fixture.mixed_export_id,
            "subscribe",
            "pending-subscribe",
            "not_subscribed",
        )
        .await
        .unwrap()
    );
    resolve_tx.commit().await.unwrap();

    let report = erase_contact(
        &maintenance_db,
        ErasureRequest {
            workspace_id: world.workspace_id,
            contact_id: fixture.erased_contact_id,
            actor_principal_id: Some(world.admin.principal_id),
            request_id: format!("t37-sync-rejected-{}", Uuid::new_v4()),
            trace_id: None,
        },
        &object_store,
        &CacheSpy,
    )
    .await
    .unwrap();
    assert_eq!(report.marketing_exports_redacted, 2);
    assert!(report.provider_cancel_not_requested);
}

#[tokio::test]
async fn unknown_native_send_blocks_erasure_until_readback_rejects_it() {
    let api_db = common::test_db().await.unwrap();
    let maintenance_db = retention_db().await;
    let world = seed_workspace(&api_db).await.unwrap();
    let fixture = create_native_fixture(&api_db, &world).await;
    let storage_dir = tempfile::tempdir().unwrap();
    let object_store =
        LocalObjectStore::new(storage_dir.path(), [9; 32], b"retention-test-key".to_vec());
    let mut tx = api_db.begin(world.workspace_id).await.unwrap();
    let export = exports::get_export(&mut tx, fixture.mixed_export_id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        exports::update_state(
            &mut tx,
            export.id,
            export.version,
            MarketingExportState::Syncing,
        )
        .await
        .unwrap()
    );
    let syncing = exports::get_export(&mut tx, export.id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        exports::update_state(
            &mut tx,
            syncing.id,
            syncing.version,
            MarketingExportState::Unknown,
        )
        .await
        .unwrap()
    );
    assert_eq!(
        exports::claim_operation(
            &mut tx,
            export.id,
            "send_campaign",
            "uncertain-send",
            &[0xb1; 32],
        )
        .await
        .unwrap(),
        OperationClaim::New
    );
    assert!(
        exports::mark_operation_unknown(
            &mut tx,
            export.id,
            "send_campaign",
            "uncertain-send",
            Some("timeout"),
        )
        .await
        .unwrap()
    );
    tx.commit().await.unwrap();

    let blocked = erase_contact(
        &maintenance_db,
        ErasureRequest {
            workspace_id: world.workspace_id,
            contact_id: fixture.erased_contact_id,
            actor_principal_id: Some(world.admin.principal_id),
            request_id: format!("t37-unknown-{}", Uuid::new_v4()),
            trace_id: None,
        },
        &object_store,
        &CacheSpy,
    )
    .await
    .expect_err("unknown provider send must be reconciled before erasure");
    assert_eq!(blocked.code, ErrorCode::Conflict);
    let mut tx = api_db.begin(world.workspace_id).await.unwrap();
    let suppression_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM suppressions
          WHERE workspace_id = $1 AND email_address_id = $2 AND purpose_scope = 'all'",
    )
    .bind(world.workspace_id)
    .bind(fixture.exclusive_email_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(
        suppression_count, 0,
        "unknown send block must not partially erase"
    );
    assert_eq!(
        exports::get_export(&mut tx, fixture.mixed_export_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        "unknown"
    );
    assert!(
        exports::reconcile_operation(
            &mut tx,
            fixture.mixed_export_id,
            "send_campaign",
            "uncertain-send",
            None,
            OperationResolution::Rejected,
            Some("not_accepted"),
        )
        .await
        .unwrap()
    );
    let unknown = exports::get_export(&mut tx, fixture.mixed_export_id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        exports::update_state(
            &mut tx,
            unknown.id,
            unknown.version,
            MarketingExportState::Rejected,
        )
        .await
        .unwrap()
    );
    tx.commit().await.unwrap();

    let report = erase_contact(
        &maintenance_db,
        ErasureRequest {
            workspace_id: world.workspace_id,
            contact_id: fixture.erased_contact_id,
            actor_principal_id: Some(world.admin.principal_id),
            request_id: format!("t37-reconciled-{}", Uuid::new_v4()),
            trace_id: None,
        },
        &object_store,
        &CacheSpy,
    )
    .await
    .unwrap();
    assert_eq!(report.marketing_exports_redacted, 2);
    assert!(report.provider_cancel_not_requested);
}
